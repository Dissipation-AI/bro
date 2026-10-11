//! The last few commands, from the zsh widget (exact, includes this session) or, when bro runs
//! as a plain command, from the end of `$HISTFILE`.

use super::RecentCommand;
use std::io::{Read, Seek, SeekFrom};

/// The last `count` non-empty commands, oldest first. Only the newest has an exit status, and
/// only when the widget passed it.
pub fn recent(from_widget: Option<Vec<String>>, last_status: Option<i32>, count: usize) -> Vec<RecentCommand> {
    let commands = from_widget.unwrap_or_else(|| read_histfile(count));
    let start = commands.len().saturating_sub(count);
    let mut recent: Vec<RecentCommand> = commands[start..]
        .iter()
        .map(|c| RecentCommand { command: c.trim().to_string(), exit_status: None, output: None })
        .collect();
    if let Some(last) = recent.last_mut() {
        last.exit_status = last_status;
    }
    recent
}

fn read_histfile(count: usize) -> Vec<String> {
    let path = std::env::var_os("HISTFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".zsh_history"));
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    const TAIL: u64 = 16 * 1024;
    if len > TAIL && file.seek(SeekFrom::Start(len - TAIL)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let mut commands = parse_histfile(&unmetafy(&bytes), len > TAIL);
    let start = commands.len().saturating_sub(count);
    commands.drain(..start);
    commands
}

/// zsh stores bytes >= 0x83 as 0x83 followed by the byte xor 0x20.
fn unmetafy(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x83 && i + 1 < bytes.len() {
            out.push(bytes[i + 1] ^ 0x20);
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse plain or EXTENDED_HISTORY (`: <time>:<duration>;command`) entries. A line ending in a
/// backslash continues on the next line. When reading from the middle of the file, the first
/// (partial) line is dropped.
fn parse_histfile(text: &str, skip_first: bool) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current: Option<String> = None;
    for (i, line) in text.split('\n').enumerate() {
        if i == 0 && skip_first {
            continue;
        }
        let line = match current.take() {
            Some(mut previous) => {
                previous.push('\n');
                previous.push_str(line);
                previous
            }
            None => strip_extended(line).to_string(),
        };
        if let Some(continued) = line.strip_suffix('\\') {
            current = Some(continued.to_string());
            continue;
        }
        if !line.trim().is_empty() {
            commands.push(line);
        }
    }
    if let Some(rest) = current {
        commands.push(rest);
    }
    commands
}

fn strip_extended(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix(": ")
        && let Some((stamp, command)) = rest.split_once(';')
        && stamp.bytes().all(|b| b.is_ascii_digit() || b == b':')
    {
        return command;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_and_multiline() {
        let text =
            ": 1700000000:0;ls -la\n: 1700000001:2;for f in *; do\\\n  echo $f\\\ndone\n\n: 1700000002:0;git status\n";
        assert_eq!(parse_histfile(text, false), vec!["ls -la", "for f in *; do\n  echo $f\ndone", "git status"]);
    }

    #[test]
    fn partial_first_line_dropped() {
        assert_eq!(parse_histfile("tus\n: 1:0;pwd\n", true), vec!["pwd"]);
    }

    #[test]
    fn metafied_bytes() {
        // "→" is 0xE2 0x86 0x92; zsh writes 0x86 and 0x92 as 0x83 followed by the byte xor 0x20.
        assert_eq!(unmetafy(&[b'a', 0xE2, 0x83, 0xA6, 0x83, 0xB2]), "a→");
    }

    #[test]
    fn widget_history_gets_status_on_newest() {
        let r = recent(Some(vec!["a".into(), "b".into(), "c".into(), "d".into()]), Some(1), 3);
        assert_eq!(r.iter().map(|c| c.command.as_str()).collect::<Vec<_>>(), ["b", "c", "d"]);
        assert_eq!(r[2].exit_status, Some(1));
        assert_eq!(r[0].exit_status, None);
    }
}
