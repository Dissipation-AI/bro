//! Output piped into bro: `cat ~/.config/bro/config.toml | bro set web search to google`.
//!
//! bro passes it through to stdout like `tee`, so you see it, and sends it to the model with the
//! request. It's also kept (redacted) for this terminal session, so later requests can use it:
//! outside CTX, this is how bro gets to see a command's output at all. The model is told it can
//! answer with `<command> | bro <request>` when it needs to look at something first.

use super::Piped;
use crate::config::cache_dir;
use std::io::{Read, Write};
use std::path::PathBuf;

/// Kept per session, newest last.
const KEPT: usize = 3;
/// Bytes kept from a long stream (the end, like the output of a command).
const MAX_BYTES: usize = 256 * 1024;
/// Older saved output is dropped.
const MAX_AGE_SECONDS: u64 = 24 * 60 * 60;

/// The terminal session: the zsh integration exports `BRO_SESSION` (its shell's pid); without
/// it, the shell that started this pipeline.
pub fn session() -> String {
    std::env::var("BRO_SESSION")
        .ok()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| std::os::unix::process::parent_id().to_string())
}

/// Copy stdin to `out` as it arrives (so a slow command shows its output live), and return what
/// was read: its last `MAX_BYTES` bytes.
pub fn tee(input: &mut impl Read, out: &mut impl Write) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut passing = true;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        // A closed reader (`| bro … | head`) stops the copy, not the request.
        if passing && (out.write_all(&buf[..n]).is_err() || out.flush().is_err()) {
            passing = false;
        }
        kept.extend_from_slice(&buf[..n]);
        if kept.len() > 2 * MAX_BYTES {
            kept.drain(..kept.len() - MAX_BYTES);
        }
    }
    if kept.len() > MAX_BYTES {
        kept.drain(..kept.len() - MAX_BYTES);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The part of a command line before `| bro`: the command whose output was piped in.
/// The zsh integration exports the line being run as `BRO_COMMAND_LINE`.
pub fn source_command(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = line.len();
    while let Some(p) = line[..i].rfind('|') {
        let after = line[p + 1..].trim_start();
        let is_bro = after.len() >= 3
            && after[..3].eq_ignore_ascii_case("bro")
            && after[3..].chars().next().is_none_or(|c| c.is_whitespace());
        let is_pipe = p == 0 || bytes[p - 1] != b'|';
        if is_bro && is_pipe {
            let before = line[..p].trim();
            return (!before.is_empty()).then(|| before.to_string());
        }
        i = p;
    }
    None
}

fn dir() -> PathBuf {
    cache_dir().join("piped")
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Earlier piped output in this session, oldest first.
pub fn earlier(session: &str) -> Vec<Piped> {
    let path = dir().join(format!("{session}.json"));
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let list: Vec<Piped> = serde_json::from_str(&text).unwrap_or_default();
    let cutoff = now().saturating_sub(MAX_AGE_SECONDS);
    list.into_iter().filter(|p| p.time >= cutoff).collect()
}

/// Remember this output for later requests in the session. Redacted before it touches the disk.
pub fn save(session: &str, piped: &Piped) {
    let mut list = earlier(session);
    list.push(Piped {
        command: piped.command.as_deref().map(|c| redact::redact(c).into_owned()),
        output: redact::redact(&piped.output).into_owned(),
        time: now(),
    });
    let excess = list.len().saturating_sub(KEPT);
    list.drain(..excess);
    let dir = dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(&list) {
        let tmp = dir.join(format!("{session}.json.tmp"));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(format!("{session}.json")));
        }
    }
    // Sessions that ended long ago.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(MAX_AGE_SECONDS);
        for e in entries.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Where a suggestion goes for the zsh integration to put in the next prompt, when bro ran as a
/// plain command in a pipeline (it can't reach the prompt itself).
pub fn pending_path(session: &str) -> PathBuf {
    cache_dir().join("pending").join(session)
}

/// Four lines: `ok` or `danger`, the command, the explanation, the request. Newlines inside a
/// field are written as 0x1f.
pub fn write_pending(session: &str, command: &str, dangerous: bool, explanation: &str, request: &str) -> bool {
    let path = pending_path(session);
    let Some(parent) = path.parent() else { return false };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let field = |s: &str| s.replace('\n', "\x1f");
    let text = format!(
        "{}\n{}\n{}\n{}\n",
        if dangerous { "danger" } else { "ok" },
        field(command),
        field(explanation),
        field(request)
    );
    std::fs::write(path, text).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_command_before_bro() {
        assert_eq!(
            source_command("cat ~/.config/bro/config.toml | bro set search to google").as_deref(),
            Some("cat ~/.config/bro/config.toml")
        );
        assert_eq!(source_command("ps aux | grep node | Bro which one").as_deref(), Some("ps aux | grep node"));
        assert_eq!(source_command("make || bro fix it"), None);
        assert_eq!(source_command("echo hi | brotli"), None);
        assert_eq!(source_command("bro hello"), None);
    }

    #[test]
    fn tee_passes_everything_through_and_keeps_the_end() {
        let input = "a".repeat(MAX_BYTES) + &"b".repeat(1000);
        let mut out = Vec::new();
        let kept = tee(&mut input.as_bytes(), &mut out);
        assert_eq!(out.len(), input.len());
        assert_eq!(kept.len(), MAX_BYTES);
        assert!(kept.ends_with("bbb"));
    }
}
