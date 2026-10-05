use super::{Limits, TrustedWorkspace, failure};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use crabber::ExtensionError;
use std::io::Read;

const OPEN: &str = "<workspace_instructions version=\"crabber-v1\">\n";
const CLOSE: &str = "</workspace_instructions>";
const TRUNCATED: &str = "\n[truncated: file exceeds the configured limit]";
const OMITTED: &str =
    "[omitted: remaining instruction files exceed the configured section limit]\n";

pub(super) fn render(
    workspace: &TrustedWorkspace,
    names: &[String],
    limits: &Limits,
    cancelled: impl Fn() -> bool,
) -> Result<Option<String>, ExtensionError> {
    if workspace.chain.len() > limits.max_chain_depth {
        return Err(failure("chain-depth"));
    }
    let mut output = String::new();
    let mut checkpoints = Vec::new();
    for (index, directory) in workspace.chain.iter().enumerate() {
        for name in names {
            if cancelled() {
                return Err(failure("cancelled"));
            }
            let Some((content, truncated)) = read(directory, name, limits.max_file_bytes) else {
                continue;
            };
            let display = format!(
                "{}{}",
                if index == workspace.chain.len() - 1 {
                    "./".into()
                } else {
                    "../".repeat(workspace.chain.len() - 1 - index)
                },
                name
            );
            let file = format!(
                "<file path=\"{}\"{}>\n{}{}\n</file>\n",
                escape(&display),
                if truncated { " truncated=\"true\"" } else { "" },
                content,
                if truncated { TRUNCATED } else { "" }
            );
            if output.is_empty() {
                output.push_str(OPEN);
            }
            if output.len() + file.len() + CLOSE.len() > limits.max_section_bytes {
                while output.len() + OMITTED.len() + CLOSE.len() > limits.max_section_bytes {
                    let checkpoint = checkpoints
                        .pop()
                        .expect("opening and omission fit minimum section limit");
                    output.truncate(checkpoint);
                }
                output.push_str(OMITTED);
                output.push_str(CLOSE);
                return Ok(Some(output));
            }
            checkpoints.push(output.len());
            output.push_str(&file);
        }
    }
    if cancelled() {
        return Err(failure("cancelled"));
    }
    if output.is_empty() {
        Ok(None)
    } else {
        output.push_str(CLOSE);
        Ok(Some(output))
    }
}

fn read(dir: &Dir, name: &str, max_bytes: usize) -> Option<(String, bool)> {
    let before = dir.symlink_metadata(name).ok()?;
    if !before.is_file() {
        return None;
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        // A raced-in FIFO cannot block the worker during open.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = dir.open_with(name, &options).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    // Only a partial trailing UTF-8 code point may be trimmed on truncation.
    if truncated && let Err(error) = std::str::from_utf8(&bytes) {
        if error.error_len().is_some() {
            return None;
        }
        bytes.truncate(error.valid_up_to());
    }
    let content = std::str::from_utf8(&bytes).ok()?.trim_end();
    if content.is_empty() || content.contains('\0') {
        return None;
    }
    Some((content.to_owned(), truncated))
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
