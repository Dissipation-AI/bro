//! Context from `ctx`, when bro runs inside a CTX tab (`CTX_TAB` set) and `ctx` is on PATH.
//! bro talks to ctx only through its command line and documented `as json` output, the same
//! way it asks git for a branch. Anything missing or unparseable is skipped.

use super::{RecentCommand, Tab};
use serde_json::Value;
use std::process::{Command, Stdio};

#[derive(Debug, Default)]
pub struct CtxContext {
    pub recent_commands: Vec<RecentCommand>,
    pub tabs: Vec<Tab>,
    pub saved_tab_commands: Vec<String>,
}

const OUTPUT_LINES: usize = 60;

pub fn read(words: &[String]) -> Option<CtxContext> {
    if !super::man::on_path("ctx") {
        return None;
    }
    let recent = std::thread::spawn(|| run(&["read", "last", "3", "commands", "as", "json"]));
    let tabs = std::thread::spawn(|| run(&["list", "as", "json"]));
    let search = find_query(words).map(|q| std::thread::spawn(move || run(&["find", &q, "as", "json"])));
    let mut c = CtxContext::default();
    if let Some(v) = recent.join().ok().flatten() {
        c.recent_commands = parse_commands(&v);
    }
    if let Some(v) = tabs.join().ok().flatten() {
        c.tabs = parse_tabs(&v);
    }
    if let Some(v) = search.and_then(|h| h.join().ok().flatten()) {
        c.saved_tab_commands = parse_found_commands(&v);
    }
    Some(c)
}

fn run(args: &[&str]) -> Option<Value> {
    let out = Command::new("ctx")
        .args(args)
        .env("CTX_ASSIST", "off")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

/// The query for saved tabs: the request's most specific word (the longest).
fn find_query(words: &[String]) -> Option<String> {
    words.iter().filter(|w| w.len() >= 4).max_by_key(|w| w.len()).cloned()
}

/// `{"commands": [{number, command, where, ran_by, exit_status, started_at, finished_at, output}]}`
/// or a bare array of those records.
fn parse_commands(v: &Value) -> Vec<RecentCommand> {
    let list = v.get("commands").unwrap_or(v);
    list.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let command = c.get("command")?.as_str()?.to_string();
                    let output = c.get("output").and_then(|o| o.as_str()).map(|o| tail_lines(o, OUTPUT_LINES));
                    let exit_status = c.get("exit_status").and_then(|s| s.as_i64()).map(|s| s as i32);
                    Some(RecentCommand { command, exit_status, output, suggested: None })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `{"windows": [{"tabs": [{number, name, folder, running, …}]}]}`
fn parse_tabs(v: &Value) -> Vec<Tab> {
    let mut tabs = Vec::new();
    for w in v.get("windows").and_then(|w| w.as_array()).into_iter().flatten() {
        for t in w.get("tabs").and_then(|t| t.as_array()).into_iter().flatten() {
            tabs.push(Tab {
                number: t.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
                name: t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                folder: t.get("folder").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                running: t.get("running").and_then(|n| n.as_str()).map(String::from),
            });
        }
    }
    tabs
}

/// Commands only, from search hits; at most three.
fn parse_found_commands(v: &Value) -> Vec<String> {
    let hits = v.get("hits").or_else(|| v.get("results")).unwrap_or(v);
    hits.as_array()
        .map(|a| a.iter().filter_map(|h| h.get("command")?.as_str().map(String::from)).take(3).collect())
        .unwrap_or_default()
}

pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_shapes() {
        let tabs: Value = serde_json::from_str(
            r#"{"windows": [{"number": 1, "name": "Work", "tabs": [
            {"number": 41, "name": "CTX spec writing", "folder": "/Users/c/w", "running": "cargo build"}]}]}"#,
        )
        .unwrap();
        let t = parse_tabs(&tabs);
        assert_eq!(t[0].number, 41);
        assert_eq!(t[0].running.as_deref(), Some("cargo build"));

        let cmds: Value = serde_json::from_str(
            r#"[{"number": 14, "command": "cargo test -p auth", "exit_status": 101, "output": "a\nb"}]"#,
        )
        .unwrap();
        let c = parse_commands(&cmds);
        assert_eq!(c[0].exit_status, Some(101));
        assert_eq!(c[0].output.as_deref(), Some("a\nb"));
    }

    #[test]
    fn searches_saved_tabs_for_the_most_specific_word() {
        assert_eq!(find_query(&crate::context::request_words("how did I deploy last time")), Some("deploy".into()));
        assert_eq!(find_query(&crate::context::request_words("run it")), None);
    }
}
