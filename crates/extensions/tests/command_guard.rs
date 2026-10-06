use crabber_extensions::command_guard::*;

#[path = "command_guard/budgets.rs"]
mod budgets;
#[path = "command_guard/contract.rs"]
mod contract;
#[path = "command_guard/corpus.rs"]
mod corpus;
#[path = "command_guard/differential.rs"]
mod differential;
#[path = "command_guard/input.rs"]
mod input;

fn limits() -> Limits {
    Limits {
        max_bindings: 8,
        max_rules: 32,
        max_rule_bytes: 2048,
        max_prefix_args: 16,
        max_json_depth: 16,
        max_json_nodes: 256,
        max_command_bytes: 4096,
        max_analysis_bytes: 8192,
        max_ast_nodes: 2048,
        max_ast_depth: 16,
        max_words: 512,
        max_word_bytes: 4096,
        max_wrapper_depth: 8,
        max_in_flight: 4,
    }
}
fn rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "blocked".into(),
            executable: "blocked".into(),
            arg_prefix: vec![],
        },
        Rule {
            id: "git-push".into(),
            executable: "git".into(),
            arg_prefix: vec!["push".into()],
        },
    ]
}
fn options() -> Options {
    Options {
        bindings: default_bindings(),
        rules: rules(),
        limits: limits(),
    }
}
fn policy() -> Policy {
    Policy::new(options()).unwrap()
}

fn generated_corpus(mut state: u64, count: usize) -> Vec<String> {
    const TOKENS: &[&str] = &[
        "blocked", "git", "push", "status", "echo", "tool", "x", "A=b", "env", "sudo", "-n",
        "timeout", "1", "command", "--", "-p", "exec", "sh", "bash", "-c", "-e", "printf", "test",
        "[", "]", "-v", "eval", "time", "$(", ")", "(", "((", "{", "}", "`", "\"", "'", "$'",
        "$\"", "\\", "$x", "${x}", "${x:-y}", "$1", "$[", "<<EOF", "<<'EOF'", "EOF", "<<<", "<",
        ">", ">>", "2>&1", "&>", "|", "|&", "||", "&&", ";", ";;", ";&", "&", "!", "#", "*", "?",
        "~", "@(", "if", "then", "fi", "for", "in", "do", "done", "case", "esac", "while", "until",
        " ", "\t", "\n", "\r", "\\\n",
    ];
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..count)
        .map(|_| {
            let n = 1 + next() % 24;
            (0..n)
                .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                .collect()
        })
        .collect()
}
#[path = "command_guard/robustness.rs"]
mod robustness;
#[cfg(unix)]
#[path = "command_guard/shell.rs"]
mod shell;
