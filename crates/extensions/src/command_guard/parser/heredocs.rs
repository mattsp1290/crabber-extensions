use super::*;

impl<'a> Parser<'a, '_, '_> {
    pub(super) fn redirect(&mut self, depth: usize) -> Result<Redirect<'a>, Outcome> {
        let (fd, op) = match self.take(depth)? {
            Token::Redirect(fd, op) => (fd, op),
            _ => return Err(Outcome::InvalidCommand),
        };
        self.budget.node()?;
        // A redirect target is a word even when digits abut another operator.
        // Lexing it as a general token would misclassify `2>&1>f`'s target.
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
        if self.peek() == Some('#') {
            return Err(Outcome::InvalidCommand);
        }
        let word = self.word(depth)?;
        if word
            .literal()
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            && matches!(self.peek(), Some('<' | '>'))
        {
            return Err(Outcome::InvalidCommand);
        }
        if op == RedirectOp::DupIn
            && word
                .literal()
                .is_some_and(|s| s != "-" && !s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(Outcome::InvalidCommand);
        }
        let target = if matches!(op, RedirectOp::Here | RedirectOp::HereTabs) {
            if self.context == Context::Substitution {
                return Err(Outcome::Unanalysable);
            }
            let (delimiter, quoted) = self.delimiter(&word.parts)?;
            let id = self.pending.len();
            self.pending.push(Pending {
                delimiter,
                quoted,
                tabs: op == RedirectOp::HereTabs,
            });
            RedirectTarget::Heredoc(id)
        } else {
            RedirectTarget::Word(word)
        };
        self.budget.node()?;
        Ok(Redirect { fd, op, target })
    }
    pub(super) fn delimiter(&self, parts: &[WordPart<'a>]) -> Result<(String, bool), Outcome> {
        let mut text = String::new();
        let mut quoted = false;
        for part in parts {
            match part {
                WordPart::Lit(s) => {
                    let mut chars = s.chars();
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            quoted = true;
                            text.push(chars.next().unwrap_or('\\'));
                        } else {
                            text.push(c);
                        }
                    }
                }
                WordPart::SglQuoted(s) => {
                    quoted = true;
                    text.push_str(s);
                }
                WordPart::DblQuoted(parts) => {
                    quoted = true;
                    for part in parts {
                        let WordPart::Lit(s) = part else {
                            return Err(Outcome::Unanalysable);
                        };
                        let mut chars = s.chars().peekable();
                        while let Some(c) = chars.next() {
                            if c == '\\' && chars.peek().is_some_and(|c| "\\$`\"".contains(*c)) {
                                if let Some(c) = chars.next() {
                                    text.push(c);
                                }
                            } else {
                                text.push(c);
                            }
                        }
                    }
                }
                _ => return Err(Outcome::Unanalysable),
            }
        }
        Ok((text, quoted))
    }
    pub(super) fn read_heredocs(&mut self, depth: usize) -> Result<(), Outcome> {
        while self.consumed < self.pending.len() {
            let pending = self
                .pending
                .get(self.consumed)
                .ok_or(Outcome::InvalidCommand)?;
            let start = self.pos;
            let mut line_start = self.pos;
            let body_end;
            loop {
                let tail = self
                    .input
                    .get(line_start..)
                    .ok_or(Outcome::InvalidCommand)?;
                let len = tail.find('\n').unwrap_or(tail.len());
                let line_end = line_start.saturating_add(len);
                let line = self.text(line_start, line_end)?;
                let compare = if pending.tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if compare == pending.delimiter {
                    body_end = line_start;
                    self.pos = line_end.saturating_add(usize::from(len < tail.len()));
                    break;
                }
                if len == tail.len() {
                    return Err(Outcome::InvalidCommand);
                }
                // Continuations in an unquoted body join physical lines before
                // delimiter matching. A joined delimiter is body data.
                if !pending.quoted
                    && line.bytes().rev().take_while(|b| *b == b'\\').count() % 2 == 1
                {
                    let mut next = line_end.saturating_add(1);
                    loop {
                        let tail = self.input.get(next..).ok_or(Outcome::InvalidCommand)?;
                        let len = tail.find('\n').unwrap_or(tail.len());
                        let s = self.text(next, next.saturating_add(len))?;
                        next = next
                            .saturating_add(len)
                            .saturating_add(usize::from(len < tail.len()));
                        if len == tail.len() {
                            return Err(Outcome::InvalidCommand);
                        }
                        if s.bytes().rev().take_while(|b| *b == b'\\').count() % 2 == 0 {
                            break;
                        }
                    }
                    line_start = next;
                } else {
                    line_start = line_end.saturating_add(1);
                }
            }
            let quoted = pending.quoted;
            let strip_tabs = pending.tabs;
            let body = self.text(start, body_end)?;
            let parts = if quoted {
                vec![]
            } else {
                let next_depth = depth.saturating_add(1);
                self.budget.depth(next_depth)?;
                let mut p = Parser {
                    input: body,
                    pos: 0,
                    dialect: self.dialect,
                    context: Context::Substitution,
                    budget: self.budget,
                    look: None,
                    pending: vec![],
                    heredocs: vec![],
                    consumed: 0,
                    strip_tabs,
                };
                p.parts(next_depth, false, true)?
            };
            self.budget.node()?;
            self.heredocs.push(Heredoc { quoted, parts });
            self.consumed = self.consumed.saturating_add(1);
        }
        Ok(())
    }
}
