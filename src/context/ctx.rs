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
}

const OUTPUT_LINES: usize = 60;
/// Commands whose output is shown (SPEC: the last 3).
const SHOWN_COMMANDS: usize = 3;
/// Saved-tab search hits kept; ctx is asked for more, since output hits are filtered out.
#[allow(dead_code)]
const SAVED_COMMANDS: usize = 3;

/// This tab's last commands with their output, and the open tabs.
pub fn read() -> Option<CtxContext> {
    if !super::man::on_path("ctx") {
        return None;
    }
    // One more than shown: when bro runs as a plain command, the newest is bro itself, unfinished.
    let recent = std::thread::spawn(|| run(&["read", "last", "4", "commands", "as", "json"]));
    let tabs = std::thread::spawn(|| run(&["list", "tabs", "as", "json"]));
    let mut c = CtxContext::default();
    if let Some(v) = recent.join().ok().flatten() {
        c.recent_commands = parse_commands(&v);
    }
    if let Some(v) = tabs.join().ok().flatten() {
        c.tabs = parse_tabs(&v);
    }
    Some(c)
}

/// Commands from other tabs, open or closed, that match the request's most specific word ("how
/// did I deploy last time"). A full-text search over every saved tab, so it's slower than
/// [`read`] and has its own, shorter, time limit.
#[allow(dead_code)] // not gathered per request (too slow); see context::gather
pub fn saved_commands(words: &[String]) -> Option<Vec<String>> {
    let query = find_query(words)?;
    if !super::man::on_path("ctx") {
        return None;
    }
    let this_tab = std::env::var("CTX_TAB").ok().and_then(|t| t.parse().ok());
    run(&["find", &query, "up", "to", "20", "results", "as", "json"]).map(|v| parse_found_commands(&v, this_tab))
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
pub fn find_query(words: &[String]) -> Option<String> {
    words.iter().filter(|w| w.len() >= 4).max_by_key(|w| w.len()).cloned()
}

/// `{"commands": [{number, command, exit_status, finished_at, full_screen, output: [lines], …}]}`
/// (ctx/docs/output.md). A command that hasn't finished (bro itself, or what's piping into it) is
/// left out, and so is the screen of a full-screen program (an editor, `top`): it's not output.
fn parse_commands(v: &Value) -> Vec<RecentCommand> {
    let list = v.get("commands").unwrap_or(v);
    let mut commands: Vec<RecentCommand> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c.get("finished_at").is_some_and(|f| !f.is_null()))
                .filter_map(|c| {
                    let command = c.get("command")?.as_str()?.to_string();
                    let full_screen = c.get("full_screen").and_then(|f| f.as_bool()).unwrap_or(false);
                    let output = c.get("output").and_then(|o| o.as_array()).filter(|_| !full_screen).map(|lines| {
                        let lines: Vec<&str> = lines.iter().filter_map(|l| l.as_str()).collect();
                        tail_lines(&lines.join("\n"), OUTPUT_LINES)
                    });
                    let exit_status = c.get("exit_status").and_then(|s| s.as_i64()).map(|s| s as i32);
                    Some(RecentCommand { command, exit_status, output, ..Default::default() })
                })
                .collect()
        })
        .unwrap_or_default();
    let start = commands.len().saturating_sub(SHOWN_COMMANDS);
    commands.drain(..start);
    commands
}

/// `{"windows": [{"tabs": [{number, name, folder, state, running, …}]}]}`
fn parse_tabs(v: &Value) -> Vec<Tab> {
    let mut tabs = Vec::new();
    for w in v.get("windows").and_then(|w| w.as_array()).into_iter().flatten() {
        for t in w.get("tabs").and_then(|t| t.as_array()).into_iter().flatten() {
            let idle = t.get("state").and_then(|s| s.as_str()) == Some("idle");
            tabs.push(Tab {
                number: t.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
                name: t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                folder: t.get("folder").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                running: t.get("running").and_then(|n| n.as_str()).filter(|_| !idle).map(String::from),
            });
        }
    }
    tabs
}

/// `{"hits": [{tab, found_in, command_text, …}]}`: commands only (a hit in a command line, not in
/// output, where a long-running program's screen would drown everything else), from other tabs,
/// without repeats; at most [`SAVED_COMMANDS`].
fn parse_found_commands(v: &Value, this_tab: Option<u64>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for h in v.get("hits").and_then(|h| h.as_array()).into_iter().flatten() {
        if h.get("found_in").and_then(|f| f.as_str()) != Some("command") {
            continue;
        }
        if this_tab.is_some() && h.get("tab").and_then(|t| t.as_u64()) == this_tab {
            continue;
        }
        let Some(text) = h.get("command_text").and_then(|c| c.as_str()) else { continue };
        if !found.iter().any(|f| f == text) {
            found.push(text.to_string());
        }
        if found.len() == SAVED_COMMANDS {
            break;
        }
    }
    found
}

pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes from ctx/docs/output.md (schema 1).
    #[test]
    fn parses_documented_shapes() {
        let tabs: Value = serde_json::from_str(
            r#"{"schema": 1, "windows": [{"number": 1, "name": "Work", "tabs": [
            {"number": 41, "name": "CTX spec writing", "folder": "/Users/c/w", "state": "busy", "running": "cargo build"},
            {"number": 42, "name": null, "folder": "/Users/c", "state": "idle", "running": "zsh"}]}]}"#,
        )
        .unwrap();
        let t = parse_tabs(&tabs);
        assert_eq!(t[0].number, 41);
        assert_eq!(t[0].running.as_deref(), Some("cargo build"));
        assert_eq!(t[1].name, "");
        assert_eq!(t[1].running, None);

        let cmds: Value = serde_json::from_str(
            r#"{"schema": 1, "tab": 212, "tab_name": null, "commands": [{"number": 14, "command": "cargo test -p auth",
            "exit_status": 101, "finished_at": "2026-10-10T17:53:11Z", "output": ["a", "b"], "events": [], "output_lines": 2}]}"#,
        )
        .unwrap();
        let c = parse_commands(&cmds);
        assert_eq!(c[0].exit_status, Some(101));
        assert_eq!(c[0].output.as_deref(), Some("a\nb"));

        // bro itself (unfinished) is left out; a full-screen program's screen isn't output.
        let cmds: Value = serde_json::from_str(
            r#"{"commands": [
            {"command": "vim notes.md", "exit_status": 0, "finished_at": "2026-10-10T17:52:40Z", "full_screen": true, "output": ["~", "~"]},
            {"command": "bro what now", "exit_status": null, "finished_at": null, "output": []}]}"#,
        )
        .unwrap();
        let c = parse_commands(&cmds);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].command, "vim notes.md");
        assert_eq!(c[0].output, None);
    }

    #[test]
    fn saved_commands_are_command_hits_from_other_tabs() {
        let hit = |tab: u64, found_in: &str, text: &str| {
            format!(r#"{{"tab": {tab}, "found_in": "{found_in}", "command": 9, "command_text": "{text}", "line": 1}}"#)
        };
        let v: Value = serde_json::from_str(&format!(
            r#"{{"schema": 1, "text": "deploy", "hits": [{}, {}, {}, {}, {}, {}], "total": 6}}"#,
            hit(25, "command", "npm run deploy -- here"),
            hit(7, "output", "claude"),
            hit(7, "command", "npm run deploy"),
            hit(8, "command", "npm run deploy"),
            hit(9, "command", "./scripts/deploy prod"),
            hit(9, "command", "fly deploy"),
        ))
        .unwrap();
        assert_eq!(parse_found_commands(&v, Some(25)), ["npm run deploy", "./scripts/deploy prod", "fly deploy"]);
    }

    #[test]
    fn searches_saved_tabs_for_the_most_specific_word() {
        assert_eq!(find_query(&crate::context::request_words("how did I deploy last time")), Some("deploy".into()));
        assert_eq!(find_query(&crate::context::request_words("run it")), None);
    }
}
