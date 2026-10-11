//! Integration tests against an in-process fake OpenAI-compatible server that records the exact
//! request and replies with a canned answer.

mod common;
use common::*;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(mut c: Command) -> Output {
    c.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// What models are asked to answer.
fn answer(command: &str, explanation: &str) -> String {
    serde_json::json!({"command": command, "explanation": explanation, "confidence": "high"}).to_string()
}

#[test]
fn plain_mode_prints_command_and_explanation() {
    let server = serve(&answer("ls -lt", "Lists files, newest first."), Duration::ZERO);
    let sb = Sandbox::new("plain", &server.url);
    let mut c = sb.command();
    c.args(["list", "files", "newest", "first"]);
    let o = run(c);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ls -lt\n");
    assert_eq!(stderr(&o), "Lists files, newest first.\n");

    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["response_format"]["json_schema"]["schema"]["required"][0], "command");
    let last = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(last.ends_with("Request: list files newest first"), "{last}");
    assert!(last.contains("Files here: notes.txt"), "{last}");
}

#[test]
fn json_mode_flags_danger() {
    let server = serve(&answer("rm -rf node_modules", "Deletes node_modules."), Duration::ZERO);
    let sb = Sandbox::new("json", &server.url);
    let mut c = sb.command();
    c.args(["--json", "delete", "node_modules"]);
    let o = run(c);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["command"], "rm -rf node_modules");
    assert_eq!(v["dangerous"], true);
    assert_eq!(v["danger_reason"], "deletes folders and everything in them");
}

#[test]
fn request_from_stdin() {
    let server = serve("du -ah . | sort -rh | head -n 5\n# The five biggest.", Duration::ZERO);
    let sb = Sandbox::new("stdin", &server.url);
    let mut c = sb.command();
    c.arg("--json").stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"list the five biggest files in this folder").unwrap();
    let o = child.wait_with_output().unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["command"], "du -ah . | sort -rh | head -n 5");
    assert_eq!(v["confidence"], "high");
}

fn zle(server: &FakeServer, name: &str, request: &str) -> String {
    let sb = Sandbox::new(name, &server.url);
    let mut c = sb.command();
    c.args(["--zle"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(request.as_bytes()).unwrap();
    let out = stdout(&child.wait_with_output().unwrap());
    // First bro's pid, so the widget can stop it on Ctrl-C.
    let (first, rest) = out.split_once('\n').unwrap();
    assert!(first.strip_prefix("pid\t").is_some_and(|p| p.parse::<u32>().is_ok()), "{out}");
    rest.to_string()
}

#[test]
fn zle_protocol_streams_the_command() {
    let server = serve(&answer("pkill -i ghostty", "Stops ghostty."), Duration::ZERO);
    let out = zle(&server, "zle", "kill that ghostty process\n");
    let lines: Vec<&str> = out.lines().collect();
    // The command grows as it streams in, is sent whole once its string closes, then the
    // explanation follows.
    assert!(lines[0].starts_with("partial\tp"), "{out}");
    let partials: Vec<&str> = lines.iter().filter(|l| l.starts_with("partial\t")).copied().collect();
    assert!(partials.len() >= 2, "{out}");
    let at = lines.iter().position(|l| *l == "command\tok\tpkill -i ghostty").expect(&out);
    assert_eq!(lines[at + 1..], ["explanation\tStops ghostty."], "{out}");

    // Multi-line commands travel as one line, newlines as 0x1f; danger is known with the command.
    let server = serve(&answer("for d in */; do\n  rm -rf \"$d\"\ndone", "Empties it."), Duration::ZERO);
    let out = zle(&server, "zle-multi", "delete every folder here");
    assert!(out.contains("command\tdanger\tfor d in */; do\x1f  rm -rf \"$d\"\x1fdone\n"), "{out}");
    assert!(out.ends_with("explanation\t⚠ deletes folders and everything in them. Empties it.\n"), "{out}");
}

#[test]
fn servers_that_dont_stream_or_lack_schemas_still_work() {
    let server = serve_with(&answer("ls -lt", "Newest first."), Duration::ZERO, None);
    assert_eq!(zle(&server, "nostream", "list files"), "command\tok\tls -lt\nexplanation\tNewest first.\n");

    let server = serve_rejecting_schema(&answer("ls -lt", "Newest first."));
    assert!(zle(&server, "noschema", "list files").contains("command\tok\tls -lt\n"));
    let sent = server.requests.lock().unwrap();
    assert_eq!(sent.len(), 2, "asked again without the schema");
    assert!(!sent[1].contains("response_format"));
}

#[test]
fn apfel_gets_no_schema() {
    // apfel's constrained decoding cuts strings at the first inner quote, so it's asked for JSON
    // in the prompt only.
    let server = serve(&answer("ls", "Lists."), Duration::ZERO);
    let sb = Sandbox::new("apfel", &server.url);
    let mut c = sb.command();
    c.args(["--model", "apple-foundationmodel", "list", "files"]);
    assert!(run(c).status.success());
    assert!(!server.requests.lock().unwrap()[0].contains("response_format"));
}

#[test]
fn secrets_never_reach_the_model() {
    let server = serve("echo ok\n# ok", Duration::ZERO);
    let sb = Sandbox::new("secrets", &server.url);
    std::fs::write(
        sb.home.join(".zsh_history"),
        ": 1700000000:0;export OPENAI_API_KEY=sk-proj-Qk3vT9xLm2Rp8WzA4bN6cY1dF7gH0jKs\n: 1700000001:0;curl -H 'Authorization: Bearer abc.def.ghi' https://api.example.com\n: 1700000002:0;why did that fail\n",
    )
    .unwrap();
    let mut c = sb.command();
    c.env("GITHUB_TOKEN", "ghp_9fK2mQ7xL3vR8tW1nB6cY4dZ0hJ5sA2pE7gU").env("MY_SECRET_THING", "hunter2hunter2");
    c.args(["--json", "why", "did", "the", "token", "fail"]);
    let o = run(c);
    assert!(o.status.success(), "{}", stderr(&o));
    let sent = server.requests.lock().unwrap().join("\n");
    for secret in ["sk-proj-Qk3vT9", "abc.def.ghi", "ghp_9fK2", "hunter2hunter2"] {
        assert!(!sent.contains(secret), "{secret} reached the model");
    }
    assert!(sent.contains("export OPENAI_API_KEY=[redacted]"), "history is still shown, redacted");
}

#[test]
fn show_prompt_sends_nothing() {
    let server = serve("x\n# x", Duration::ZERO);
    let sb = Sandbox::new("showprompt", &server.url);
    let mut c = sb.command();
    c.args(["--show-prompt", "list", "files"]);
    let o = run(c);
    assert!(stdout(&o).contains("Request: list files"));
    assert!(stdout(&o).contains("nothing was sent"));
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn unreachable_endpoint_explains() {
    let sb = Sandbox::new("down", "http://127.0.0.1:9/v1");
    let mut c = sb.command();
    c.args(["list", "files"]);
    let o = run(c);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("can't reach the model at http://127.0.0.1:9/v1"), "{}", stderr(&o));
}

#[test]
fn prose_answer_is_not_a_command() {
    let server = serve("I'm sorry, I can't help with that.", Duration::ZERO);
    let sb = Sandbox::new("prose", &server.url);
    let mut c = sb.command();
    c.args(["--zle", "do", "something"]);
    let o = run(c);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).lines().nth(1).unwrap().starts_with("error\tbro: the model gave no command"), "{}", stdout(&o));
}

#[test]
fn subcommands_only_when_alone() {
    let server = serve("git config --global user.email me@example.com\n# Sets your email.", Duration::ZERO);
    let sb = Sandbox::new("subcommands", &server.url);
    let mut c = sb.command();
    c.args(["config", "my", "git", "email"]);
    let o = run(c);
    assert!(stdout(&o).starts_with("git config"), "a request, not `bro config`: {}", stdout(&o));
    let mut c = sb.command();
    c.args(["init", "zsh"]);
    let o = run(c);
    assert!(stdout(&o).contains("_bro_accept_line"));
}

#[test]
fn learning_round_trip() {
    let server = serve("code ~/notes\n# Opens notes.", Duration::ZERO);
    let sb = Sandbox::new("learning", &server.url);
    let mut rec = sb.command();
    rec.arg("--record")
        .env("BRO_REC_REQUEST", "open my notes")
        .env("BRO_REC_SUGGESTED", "open -a Notes")
        .env("BRO_REC_RAN", "code ~/notes")
        .env("BRO_REC_STATUS", "0");
    assert!(run(rec).status.success());
    let mut c = sb.command();
    c.args(["open", "the", "notes"]);
    run(c);
    let sent = server.requests.lock().unwrap().join("\n");
    assert!(sent.contains("code ~/notes"), "learned example sent: {sent}");
}

/// bro's own overhead, model excluded: process start to answer printed, with an instant server.
#[test]
fn overhead_is_small() {
    let server = serve("ls\n# Lists files.", Duration::ZERO);
    let sb = Sandbox::new("overhead", &server.url);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let mut c = sb.command();
        c.args(["list", "files"]);
        let t = Instant::now();
        let o = run(c);
        best = best.min(t.elapsed());
        assert!(o.status.success());
    }
    // Generous for debug builds and loaded CI machines; release runs take a few milliseconds.
    assert!(best < Duration::from_millis(150), "best of 5 took {best:?}");
}
