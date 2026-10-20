//! Learning from what you actually ran. After bro fills the prompt, the zsh integration records
//! the request, the suggestion, what you ran instead (maybe edited) and its exit status, in
//! `~/Library/Application Support/bro/history.jsonl`. Similar requests later get those pairs as
//! examples.
//!
//! A request whose suggestion wasn't run, followed shortly by a reworded request whose command
//! was, counts as an example for both wordings: that's how retries teach bro.

use crate::config::data_dir;
use serde::{Deserialize, Serialize};
use std::io::Write;

const RETRY_WINDOW_SECONDS: u64 = 180;
const MAX_FILE_BYTES: u64 = 1_000_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub time: u64,
    pub folder: String,
    pub request: String,
    pub suggested: String,
    /// bro's explanation of the suggestion, as shown under the prompt.
    pub explanation: String,
    /// The next command line after the suggestion, `None` if the next thing was another request.
    pub ran: Option<String>,
    pub exit_status: Option<i32>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Example {
    pub request: String,
    pub command: String,
}

fn history_path() -> std::path::PathBuf {
    data_dir().join("history.jsonl")
}

/// Append a record (redacted). Called by the zsh integration in the background.
pub fn record(mut r: Record) -> Result<(), String> {
    let clean = |s: &str| redact::redact(s).into_owned();
    r.request = clean(&r.request);
    r.suggested = clean(&r.suggested);
    r.explanation = clean(&r.explanation);
    r.ran = r.ran.as_deref().map(clean);
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = history_path();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
        rotate(&path);
    }
    let mut line = serde_json::to_string(&r).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// Keep the newest half of the file.
fn rotate(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines[lines.len() / 2..].join("\n") + "\n";
    let tmp = path.with_extension("jsonl.tmp");
    if std::fs::write(&tmp, keep).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn read_records() -> Vec<Record> {
    let Ok(text) = std::fs::read_to_string(history_path()) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The request in a history line that asked bro (`bro find big files`, or `# bro …` as older
/// versions left it), if it is one. bro's own subcommands and flags (`bro config`) are commands,
/// not requests.
pub fn asked(command: &str) -> Option<&str> {
    let line = command.trim();
    let rest = line.strip_prefix('#').map(str::trim_start).unwrap_or(line);
    let lower = rest.get(..3)?.to_ascii_lowercase();
    let after = rest.get(3..)?;
    if lower != "bro" || !after.chars().next().is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation()) {
        return None;
    }
    let request = after.trim_start_matches(|c: char| c.is_ascii_punctuation()).trim();
    let own = request.is_empty() || request.starts_with('-') || crate::SUBCOMMANDS.contains(&request);
    (!own).then_some(request)
}

/// Next to each earlier request in the history, what bro suggested for it (newest record).
pub fn annotate(recent: &mut [super::RecentCommand]) {
    if !recent.iter().any(|r| asked(&r.command).is_some()) {
        return;
    }
    let records = read_records();
    for r in recent.iter_mut() {
        let Some(request) = asked(&r.command) else { continue };
        let request = redact::redact(request).to_lowercase();
        let found = records.iter().rev().find(|rec| rec.request.trim().to_lowercase() == request);
        r.suggested = found.map(|rec| rec.suggested.clone()).filter(|s| !s.is_empty());
        r.suggested_explanation = found.map(|rec| rec.explanation.clone()).filter(|s| !s.is_empty());
    }
}

/// Turn records into (request → command that worked) pairs, oldest first.
pub fn examples_from(records: &[Record]) -> Vec<Example> {
    let mut examples = Vec::new();
    for (i, r) in records.iter().enumerate() {
        let Some(ran) = r.ran.as_deref().filter(|s| !s.trim().is_empty()) else { continue };
        if r.exit_status.is_some_and(|s| s != 0) {
            continue;
        }
        examples.push(Example { request: r.request.clone(), command: ran.to_string() });
        // Earlier wordings of the same wish whose suggestions weren't used.
        let mut later = r.time;
        for earlier in records[..i].iter().rev() {
            if earlier.ran.is_some() || later.saturating_sub(earlier.time) > RETRY_WINDOW_SECONDS {
                break;
            }
            examples.push(Example { request: earlier.request.clone(), command: ran.to_string() });
            later = earlier.time;
        }
    }
    examples
}

/// Up to `limit` examples sharing words with the request, best match first (newest on ties).
pub fn relevant(words: &[String], limit: usize) -> Vec<Example> {
    rank(examples_from(&read_records()), words, limit)
}

fn rank(examples: Vec<Example>, words: &[String], limit: usize) -> Vec<Example> {
    let mut scored: Vec<(usize, usize, Example)> = examples
        .into_iter()
        .enumerate()
        .filter_map(|(age, e)| {
            let theirs = super::request_words(&e.request);
            let score = words.iter().filter(|w| theirs.contains(w)).count();
            (score > 0).then_some((score, age, e))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out: Vec<Example> = Vec::new();
    for (_, _, e) in scored {
        if !out.iter().any(|o| o.request == e.request && o.command == e.command) {
            out.push(e);
        }
        if out.len() == limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_in_history() {
        assert_eq!(asked("bro find big files"), Some("find big files"));
        assert_eq!(asked("Bro, what's on port 3000?"), Some("what's on port 3000?"));
        assert_eq!(asked("# bro fix it"), Some("fix it"));
        assert_eq!(asked("bro config"), None);
        assert_eq!(asked("bro --show-prompt list files"), None);
        assert_eq!(asked("brown paper"), None);
        assert_eq!(asked("cat x | bro fix"), None);
    }

    fn rec(time: u64, request: &str, ran: Option<&str>, status: Option<i32>) -> Record {
        Record {
            time,
            request: request.into(),
            suggested: "x".into(),
            ran: ran.map(String::from),
            exit_status: status,
            ..Record::default()
        }
    }

    #[test]
    fn retries_teach_both_wordings() {
        let records = vec![
            rec(100, "open my sysadmin agent", None, None),
            rec(130, "start the sys-admin agent thing", Some("~/Agents/sys-admin/bin/sysadmin-agent"), Some(0)),
            rec(5000, "list big files", Some("du -ah . | sort -rh | head"), Some(1)),
        ];
        let ex = examples_from(&records);
        assert_eq!(ex.len(), 2);
        assert!(ex.iter().any(|e| e.request == "open my sysadmin agent" && e.command.contains("sysadmin-agent")));
    }

    #[test]
    fn ranking_prefers_overlap() {
        let ex = vec![
            Example { request: "open safari".into(), command: "open -a Safari".into() },
            Example { request: "kill ghostty".into(), command: "pkill -i ghostty".into() },
        ];
        let words = crate::context::request_words("kill that ghostty process");
        let r = rank(ex, &words, 5);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].command, "pkill -i ghostty");
    }
}
