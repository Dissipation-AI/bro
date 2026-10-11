//! Turns a [`Context`] into chat messages for a small model:
//!
//! 1. instructions, with only the rules that apply to this request (`[tag]` lines in
//!    `prompts/system.md`);
//! 2. up to three examples as earlier turns: first what this person ran for similar requests,
//!    then the most similar built-in examples;
//! 3. every context source, highest priority first, each within its own size limit, then the
//!    request. The whole is trimmed to the budget ([`Config::budget`]) from the lowest priority
//!    up. Redaction runs last, on every message.
//!
//! The model answers in JSON: `{"command", "explanation", "confidence"}`.

use crate::config::Config;
use crate::context::{self, Context};
use serde::Deserialize;

const SYSTEM: &str = include_str!("../prompts/system.md");
const EXAMPLES: &str = include_str!("../prompts/examples.toml");
/// Examples shown. Apple's guidance for its on-device model is fewer than five.
const SHOWN_EXAMPLES: usize = 3;
const LEARNED_EXAMPLES: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
}

#[derive(Deserialize)]
struct Bank {
    example: Vec<BuiltIn>,
}

#[derive(Deserialize, Clone)]
struct BuiltIn {
    #[serde(default)]
    context: String,
    #[serde(default)]
    line: String,
    request: String,
    command: String,
    explanation: String,
}

/// Rough token count: about 3 characters per token for this mix of English, paths and shell
/// (measured against apfel's own counts; it errs high, so budgets hold).
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 3 + 1
}

/// How the model is asked to answer (SPEC: JSON with command, explanation, confidence);
/// `llm::parse` reads it back.
pub fn answer_text(command: &str, explanation: &str) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    format!("{{\"command\": {}, \"explanation\": {}, \"confidence\": \"high\"}}", q(command), q(explanation))
}

/// The request's words, for matching names in the context and picking examples. Which rules and
/// examples apply is decided by what the context holds, never by lists of keywords.
struct Topics {
    words: Vec<String>,
}

impl Topics {
    fn new(c: &Context) -> Topics {
        Topics { words: context::request_words(&format!("{} {}", c.request, c.line)) }
    }
}

pub fn build(context: &Context, config: &Config) -> Vec<Message> {
    let topics = Topics::new(context);
    let last_failed = context.recent_commands.last().and_then(|r| r.exit_status).is_some_and(|s| s != 0);
    let mut system = system_prompt(config, context, last_failed);
    if let Some(notes) = notes(context, config) {
        system.push_str("\n\nThis person's own notes for you, from ~/.config/bro/bro.md. Follow them; they come before the rules above:\n");
        system.push_str(&notes);
    }
    let mut messages = vec![Message { role: "system", content: system }];

    for Shown { context: ex_context, line, request, command, explanation } in examples(context, config) {
        messages.push(Message { role: "user", content: user_text(&ex_context, &line, &request) });
        messages.push(Message { role: "assistant", content: answer_text(&command, &explanation) });
    }

    let fixed: usize = messages.iter().map(|m| estimate_tokens(&m.content)).sum::<usize>()
        + estimate_tokens(&context.request)
        + estimate_tokens(&context.line)
        + 20;
    let budget = config.budget().saturating_sub(fixed);
    let sections = fit(sections(context, &topics, config), budget);
    messages.push(Message { role: "user", content: user_text(&sections.join("\n"), &context.line, &context.request) });

    for m in &mut messages {
        m.content = redact::redact(&m.content).into_owned();
    }
    messages
}

/// bro.md, cut to whole lines within its share of the budget.
fn notes(c: &Context, config: &Config) -> Option<String> {
    let max_tokens = if config.small() { 400 } else { 1_500 };
    let mut out = String::new();
    for line in c.notes.trim().lines() {
        if estimate_tokens(&out) + estimate_tokens(line) > max_tokens {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    let out = out.trim_end().to_string();
    (!out.is_empty()).then_some(out)
}

/// Instructions with only the `[tag]` rules that apply.
fn system_prompt(config: &Config, c: &Context, last_failed: bool) -> String {
    let search_rule = if config.browser.is_empty() || config.browser == "default" {
        format!("open \"{}\"", config.search_url.replace("{query}", "WORDS"))
    } else {
        format!("open -a \"{}\" \"{}\"", config.browser, config.search_url.replace("{query}", "WORDS"))
    };
    let mut out = String::new();
    for line in SYSTEM.trim().lines() {
        let keep = match line.strip_prefix('[').and_then(|l| l.split_once("] ")) {
            None => Some(line.to_string()),
            Some((tag, rule)) => {
                let applies = match tag {
                    "failed" => last_failed,
                    "line" => !c.line.trim().is_empty(),
                    "tabs" => !c.tabs.is_empty(),
                    "manual" => !c.manuals.is_empty(),
                    "programs" => !c.programs.is_empty(),
                    "learned" => !c.examples.is_empty(),
                    "piped" => c.piped.is_some(),
                    _ => true,
                };
                applies.then(|| rule.to_string())
            }
        };
        if let Some(l) = keep {
            out.push_str(&l.replace("open \"{search_url}\"", &search_rule));
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

fn user_text(context: &str, line: &str, request: &str) -> String {
    let mut out = String::new();
    if !context.trim().is_empty() {
        out.push_str(context.trim_end());
        out.push_str("\n\n");
    }
    if !line.trim().is_empty() {
        out.push_str("Typed so far: ");
        out.push_str(line.trim());
        out.push('\n');
    }
    out.push_str("Request: ");
    out.push_str(request.trim());
    out
}

#[derive(Debug)]
struct Shown {
    context: String,
    line: String,
    request: String,
    command: String,
    explanation: String,
}

/// Examples to show: this person's own first, then built-ins.
fn examples(c: &Context, config: &Config) -> Vec<Shown> {
    let mut out: Vec<Shown> = c
        .examples
        .iter()
        .take(LEARNED_EXAMPLES)
        .map(|e| Shown {
            context: String::new(),
            line: String::new(),
            request: e.request.clone(),
            command: e.command.clone(),
            explanation: "what you ran last time for this".into(),
        })
        .collect();
    for ex in pick_builtin(c, config) {
        if out.len() >= SHOWN_EXAMPLES {
            break;
        }
        out.push(Shown {
            context: ex.context,
            line: ex.line,
            request: ex.request,
            command: ex.command,
            explanation: ex.explanation,
        });
    }
    // Shown oldest-first, so the most similar example sits right before the real request.
    out.reverse();
    out
}

/// Kinds of situation an example demonstrates, so a request gets examples of the same shape
/// even when it shares no words with them ("open my sysadmin agent" ~ "start my backup tool").
#[derive(PartialEq)]
enum Shape {
    Programs,
    Tabs,
    Processes,
    Failed,
    Line,
    Piped,
}

fn shapes_of_request(c: &Context) -> Vec<Shape> {
    let words = Topics::new(c).words;
    let mut v = Vec::new();
    if !c.programs.is_empty() {
        v.push(Shape::Programs);
    }
    if !c.tabs.is_empty() {
        v.push(Shape::Tabs);
    }
    if c.processes.iter().any(|p| context::relevant(&p.name, &words)) {
        v.push(Shape::Processes);
    }
    if c.recent_commands.last().and_then(|r| r.exit_status).is_some_and(|s| s != 0) {
        v.push(Shape::Failed);
    }
    if !c.line.trim().is_empty() {
        v.push(Shape::Line);
    }
    if c.piped.is_some() {
        v.push(Shape::Piped);
    }
    v
}

fn shapes_of_example(ex: &BuiltIn) -> Vec<Shape> {
    let mut v = Vec::new();
    if ex.context.contains("Programs you can run") {
        v.push(Shape::Programs);
    }
    if ex.context.contains("Open tabs") {
        v.push(Shape::Tabs);
    }
    if ex.context.contains("Running processes") {
        v.push(Shape::Processes);
    }
    if ex.context.contains("exit status") && !ex.context.contains("exit status 0") {
        v.push(Shape::Failed);
    }
    if !ex.line.is_empty() {
        v.push(Shape::Line);
    }
    if ex.context.contains("Piped into bro") {
        v.push(Shape::Piped);
    }
    v
}

fn pick_builtin(c: &Context, config: &Config) -> Vec<BuiltIn> {
    let bank: Bank = toml::from_str(EXAMPLES).expect("prompts/examples.toml is valid");
    // Split file names too (`app.log` matches "log"), so examples match on what things are.
    let stems = |text: &str| -> Vec<String> {
        context::request_words(&text.replace(['.', '/'], " ")).iter().map(|w| context::stem(w)).collect()
    };
    let words = stems(&format!("{} {}", c.request, c.line));
    let shapes = shapes_of_request(c);
    let mut scored: Vec<(usize, usize, BuiltIn)> = bank
        .example
        .into_iter()
        .enumerate()
        .map(|(i, ex)| {
            let theirs = stems(&format!("{} {} {}", ex.request, ex.line, ex.command));
            let overlap = words.iter().filter(|w| theirs.contains(w)).count();
            let same_shape = shapes_of_example(&ex).iter().filter(|s| shapes.contains(s)).count();
            (overlap + 2 * same_shape, i, ex)
        })
        .collect();
    // Best overlap first; the bank's own order breaks ties, so results are deterministic.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // Unrelated examples only prime the model (a `pkill` example invites killing), so none
    // rather than a weak one.
    scored
        .into_iter()
        .filter(|(score, _, _)| *score > 0)
        .take(SHOWN_EXAMPLES)
        .map(|(_, _, mut ex)| {
            ex.command = expand_search(&ex.command, config);
            ex
        })
        .collect()
}

/// `{search:some words}` → the configured search command's URL for those words.
fn expand_search(text: &str, config: &Config) -> String {
    let Some(start) = text.find("{search:") else { return text.to_string() };
    let Some(len) = text[start..].find('}') else { return text.to_string() };
    let words = &text[start + 8..start + len];
    let query: String = words.split_whitespace().map(url_encode).collect::<Vec<_>>().join("+");
    let url = config.search_url.replace("{query}", &query);
    let out = format!("{}{}{}", &text[..start], url, &text[start + len + 1..]);
    if config.browser.is_empty() || config.browser == "default" {
        out
    } else {
        out.replacen("open ", &format!("open -a \"{}\" ", config.browser), 1)
    }
}

fn url_encode(word: &str) -> String {
    word.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Size limits per source, in tokens. With a small window (apfel) sources are trimmed hard
/// (SPEC: the last command's output at ~20 lines, the top 5 processes, 5 past requests).
struct Limits {
    output_lines: usize,
    /// Lines of text piped into this request: it was sent on purpose, so more of it is kept.
    piped_lines: usize,
    outputs_shown: usize,
    processes: usize,
    examples: usize,
    environment: usize,
    names: usize,
    apps: usize,
    projects: usize,
    tabs: usize,
}

impl Limits {
    fn for_config(config: &Config) -> Limits {
        if config.small() {
            Limits {
                output_lines: 20,
                piped_lines: 60,
                outputs_shown: 1,
                processes: 5,
                examples: 5.min(config.history_examples),
                environment: 120,
                names: 60,
                apps: 120,
                projects: 120,
                tabs: 200,
            }
        } else {
            Limits {
                output_lines: 60,
                piped_lines: 200,
                outputs_shown: 3,
                processes: 30,
                examples: config.history_examples,
                environment: 400,
                names: 200,
                apps: 400,
                projects: 400,
                tabs: 600,
            }
        }
    }
}

/// `items` joined with `sep` after `header`, as many as fit in `max_tokens`.
fn capped(header: &str, items: impl IntoIterator<Item = String>, sep: &str, max_tokens: usize) -> Option<String> {
    let mut out = header.to_string();
    let mut any = false;
    for item in items {
        let piece = if any { format!("{sep}{item}") } else { item };
        if estimate_tokens(&out) + estimate_tokens(&piece) > max_tokens {
            break;
        }
        out.push_str(&piece);
        any = true;
    }
    any.then_some(out)
}

/// Every source, highest priority first (SPEC § Context it gathers); `fit` trims from the end.
fn sections(c: &Context, topics: &Topics, config: &Config) -> Vec<String> {
    let words = &topics.words;
    let limits = Limits::for_config(config);
    let mut out = Vec::new();

    if let Some(p) = &c.piped {
        let mut s = match &p.command {
            Some(command) => format!("Piped into bro, the output of `{command}`:"),
            None => "Piped into bro:".to_string(),
        };
        push_indented(&mut s, &head_and_tail(&p.output, limits.piped_lines));
        out.push(s);
    }

    if !c.recent_commands.is_empty() {
        let mut s = String::from("Commands already run here, oldest first:");
        let first_output = c.recent_commands.len().saturating_sub(limits.outputs_shown);
        for (i, r) in c.recent_commands.iter().enumerate() {
            s.push_str("\n$ ");
            s.push_str(&r.command.replace('\n', "\n  "));
            if let Some(status) = r.exit_status {
                s.push_str(&format!("\n  exit status {status}"));
            }
            if i >= first_output
                && let Some(output) = r.output.as_deref().filter(|o| !o.trim().is_empty())
            {
                s.push_str("\n  output:");
                for l in context::ctx::tail_lines(output, limits.output_lines).lines() {
                    s.push_str("\n    ");
                    s.push_str(l);
                }
            }
        }
        out.push(s);
    }

    if !c.earlier_piped.is_empty() {
        let mut s = String::from("Output piped into bro earlier in this terminal, oldest first:");
        for p in &c.earlier_piped {
            s.push_str(&format!("\n$ {} | bro", p.command.as_deref().unwrap_or("(a command)")));
            push_indented(&mut s, &context::ctx::tail_lines(&p.output, limits.output_lines));
        }
        out.push(s);
    }

    if !c.folder.is_empty() {
        let mut s = format!("Current folder: {}", c.folder);
        if let Some(git) = &c.git {
            s.push_str(&format!(" (git branch {}", git.branch));
            if let Some(status) = &git.status {
                s.push_str(&format!(", {status}"));
            }
            s.push(')');
        }
        if !c.files.is_empty() {
            s.push_str(&format!("\nFiles here: {}", c.files.join(", ")));
            if c.files_total > c.files.len() {
                s.push_str(&format!(" (first {} of {})", c.files.len(), c.files_total));
            }
        } else {
            s.push_str("\nFiles here: none");
        }
        out.push(s);
    }

    if let Some(s) = capped(
        "Open tabs:",
        c.tabs.iter().map(|t| {
            let mut line = format!("\n  tab {} \"{}\" in {}", t.number, t.name, t.folder);
            if let Some(r) = &t.running {
                line.push_str(&format!(", running {r}"));
            }
            line
        }),
        "",
        limits.tabs,
    ) {
        out.push(s);
    }

    if !c.processes.is_empty() {
        // Processes named in the request come first and are always kept; then the busiest.
        let matching = c.processes.iter().filter(|p| context::relevant(&p.name, words)).count();
        let mut s = String::from("Running processes (for reference; only act on one if asked):");
        for p in c.processes.iter().take(matching + limits.processes) {
            s.push_str(&format!("\n  {} (pid {})", p.name, p.pid));
            if let Some(cpu) = p.cpu_percent {
                let when = if p.cpu_average { " on average" } else { "" };
                s.push_str(&format!(" cpu {cpu}%{when}"));
            }
            if let Some(m) = p.memory_mb {
                s.push_str(&format!(", memory {m} MB"));
            }
        }
        out.push(s);
    }
    if !c.jobs.is_empty() {
        out.push(format!("Shell jobs: {}", c.jobs.join("; ")));
    }

    // Variables named like the request first, then the rest.
    let mut env: Vec<&(String, Option<String>)> = c.environment.iter().collect();
    env.sort_by_key(|(name, _)| !context::relevant(name, words));
    if let Some(s) = capped(
        "Environment: ",
        env.into_iter().map(|(name, value)| match value {
            Some(v) => format!("{name}={v}"),
            None => format!("{name} (set)"),
        }),
        ", ",
        limits.environment,
    ) {
        out.push(s);
    }
    let mut names: Vec<&String> = c.aliases.iter().chain(&c.functions).collect();
    names.sort_by_key(|n| !context::relevant(n, words));
    if let Some(s) = capped("Aliases and functions: ", names.into_iter().cloned(), ", ", limits.names) {
        out.push(s);
    }

    // Named in the request first (the live index is already in this order; snapshots may not be).
    let by_relevance = |items: &[String]| -> Vec<String> {
        let mut v = items.to_vec();
        v.sort_by_key(|i| !context::relevant(i.rsplit('/').next().unwrap_or(i), words));
        v
    };
    if let Some(mut s) = capped("Apps: ", by_relevance(&c.apps), ", ", limits.apps) {
        if let Some(b) = &c.default_browser {
            s.push_str(&format!("\nDefault browser: {b}"));
        }
        out.push(s);
    }
    if !c.programs.is_empty() {
        out.push(format!("Programs you can run: {}", c.programs.join(", ")));
    }
    if let Some(s) = capped("Project folders: ", by_relevance(&c.projects), ", ", limits.projects) {
        out.push(s);
    }

    if !c.examples.is_empty() {
        let mut s = String::from("What this person ran before, most relevant first:");
        for e in c.examples.iter().take(limits.examples) {
            s.push_str(&format!("\n  for \"{}\" they ran: {}", e.request, e.command));
        }
        out.push(s);
    }
    if !c.saved_tab_commands.is_empty() {
        out.push(format!("Commands from saved tabs: {}", c.saved_tab_commands.join(" ; ")));
    }

    for m in &c.manuals {
        out.push(format!("Manual excerpt for {} on this Mac:\n{}", m.command, m.text.trim_end()));
    }

    let sys = &c.system;
    let mut about: Vec<String> = Vec::new();
    if !sys.macos.is_empty() {
        about.push(format!("macOS {}", sys.macos));
    }
    about.extend([&sys.arch, &sys.shell, &sys.terminal].into_iter().filter(|p| !p.is_empty()).cloned());
    if !about.is_empty() {
        out.push(format!("This Mac: {}", about.join(", ")));
    }
    out
}

fn push_indented(s: &mut String, text: &str) {
    for l in text.lines() {
        s.push_str("\n    ");
        s.push_str(l);
    }
}

/// The first and last lines of a long text, with a note of how many were skipped.
fn head_and_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return lines.join("\n");
    }
    let head = max_lines / 2;
    let tail = max_lines - head;
    format!(
        "{}\n… ({} lines skipped)\n{}",
        lines[..head].join("\n"),
        lines.len() - head - tail,
        lines[lines.len() - tail..].join("\n")
    )
}

/// Keep sections in priority order while they fit; a section that doesn't fit is shortened to
/// whole lines if that leaves something useful, otherwise dropped.
fn fit(sections: Vec<String>, budget: usize) -> Vec<String> {
    let mut used = 0;
    let mut kept = Vec::new();
    for s in sections {
        let cost = estimate_tokens(&s) + 1;
        if used + cost <= budget {
            used += cost;
            kept.push(s);
            continue;
        }
        let mut partial = String::new();
        for line in s.lines() {
            if used + estimate_tokens(&partial) + estimate_tokens(line) + 1 > budget {
                break;
            }
            if !partial.is_empty() {
                partial.push('\n');
            }
            partial.push_str(line);
        }
        if partial.lines().count() >= 2 {
            used += estimate_tokens(&partial) + 1;
            kept.push(partial);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Process, RecentCommand, Tab};

    fn ctx(request: &str) -> Context {
        Context { request: request.into(), folder: "~/a".into(), ..Context::default() }
    }

    #[test]
    fn search_template() {
        let config = Config::default();
        assert_eq!(
            expand_search("open \"{search:salsa dancing & más}\"", &config),
            "open \"https://search.brave.com/search?q=salsa+dancing+%26+m%C3%A1s\""
        );
        let brave = Config { browser: "Brave Browser".into(), ..Config::default() };
        assert!(expand_search("open \"{search:x}\"", &brave).starts_with("open -a \"Brave Browser\" \"https://"));
    }

    #[test]
    fn examples_are_relevant_and_learned_first() {
        let mut c = ctx("kill that ghostty process");
        c.processes = vec![Process {
            name: "ghostty".into(),
            pid: 812,
            cpu_percent: Some(1.0),
            memory_mb: Some(600),
            ..Default::default()
        }];
        let ex = examples(&c, &Config::default());
        assert!(!ex.is_empty());
        assert!(ex.last().unwrap().command.starts_with("pkill"), "most similar example last: {ex:?}");
        c.examples =
            vec![context::learned::Example { request: "kill ghostty".into(), command: "killall ghostty".into() }];
        let ex = examples(&c, &Config::default());
        assert_eq!(ex.last().unwrap().command, "killall ghostty");
    }

    #[test]
    fn rules_only_when_they_apply() {
        let config = Config::default();
        let plain = build(&ctx("list files"), &config);
        // Rules about kinds of context appear only when that context is there; never by keyword.
        assert!(!plain[0].content.contains("ctx show"));
        assert!(!plain[0].content.contains("Typed so far"));
        let mut c = ctx("show me the dev server tab");
        c.tabs = vec![Tab { number: 42, name: "Dev server".into(), folder: "~".into(), running: None }];
        let m = build(&c, &config);
        assert!(m[0].content.contains("ctx show"));
        assert!(m.last().unwrap().content.contains("tab 42 \"Dev server\""));
    }

    #[test]
    fn piped_text_comes_first_with_its_rule() {
        let mut c = ctx("set the web search to google");
        c.piped = Some(crate::context::Piped {
            command: Some("cat ~/.config/bro/config.toml".into()),
            output: (1..=300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
            time: 0,
        });
        c.folder = "~/code".into();
        let m = build(&c, &Config::default());
        let user = &m.last().unwrap().content;
        assert!(user.starts_with("Piped into bro, the output of `cat ~/.config/bro/config.toml`:"), "{user}");
        assert!(user.contains("line 1\n") && user.contains("line 300") && user.contains(" lines skipped)"), "{user}");
        assert!(m[0].content.contains("Don't pipe it into bro again"));

        let m = build(&ctx("change your default web search to google"), &Config::default());
        assert!(m[0].content.contains("~/.config/bro/config.toml"));
        assert!(!m[0].content.contains("Don't pipe it into bro again"));
    }

    #[test]
    fn notes_from_bro_md_are_followed_within_their_budget() {
        let mut c = ctx("connect to the gpu box");
        c.notes = "My GPU box is `ssh lab-gpu`.\n".to_string() + &"- filler line\n".repeat(2000);
        let m = build(&c, &Config::default());
        assert!(m[0].content.contains("notes for you, from ~/.config/bro/bro.md"));
        assert!(m[0].content.contains("My GPU box is `ssh lab-gpu`."));
        assert!(estimate_tokens(&m[0].content) < 2_000, "bro.md is capped");
        assert!(!build(&ctx("list files"), &Config::default())[0].content.contains("notes for you"));
    }

    #[test]
    fn every_source_is_included() {
        let mut c = ctx("zip the src folder");
        c.processes = vec![Process { name: "node".into(), pid: 1, ..Default::default() }];
        c.environment = vec![("EDITOR".into(), Some("vim".into())), ("GITHUB_TOKEN".into(), None)];
        c.aliases = vec!["ll".into()];
        c.apps = vec!["Safari".into()];
        c.default_browser = Some("Safari".into());
        c.projects = vec!["~/code/site".into()];
        c.system = context::system::System { macos: "26.1".into(), arch: "arm64".into(), ..Default::default() };
        let last = build(&c, &Config::default()).pop().unwrap().content;
        for expected in [
            "node (pid 1)",
            "EDITOR=vim, GITHUB_TOKEN (set)",
            "Aliases and functions: ll",
            "Apps: Safari",
            "Default browser: Safari",
            "Project folders: ~/code/site",
            "This Mac: macOS 26.1, arm64",
        ] {
            assert!(last.contains(expected), "{expected} missing from:\n{last}");
        }
    }

    #[test]
    fn small_windows_keep_the_top_five_processes_plus_named_ones() {
        let mut c = ctx("kill ghostty");
        c.processes = (0..20).map(|i| Process { name: format!("proc{i}"), pid: i, ..Default::default() }).collect();
        c.processes.insert(0, Process { name: "ghostty".into(), pid: 99, ..Default::default() });
        let shown = |config: &Config| {
            let last = build(&c, config).pop().unwrap().content;
            last.lines().filter(|l| l.contains("(pid ")).count()
        };
        assert_eq!(shown(&Config::default()), 6, "apfel: ghostty plus the top 5");
        let big = Config { model: "qwen3-8b".into(), ..Config::default() };
        assert_eq!(shown(&big), 21, "6,000 tokens: all of them (up to 30)");
    }

    #[test]
    fn request_comes_last_and_is_redacted() {
        let mut c = ctx("why did that fail");
        c.recent_commands = vec![RecentCommand {
            command: "export OPENAI_API_KEY=sk-proj-Qk3vT9xLm2Rp8WzA4bN6cY1dF7gH0jKs".into(),
            exit_status: Some(1),
            output: Some("boom".into()),
        }];
        let m = build(&c, &Config::default());
        let last = m.last().unwrap();
        assert!(last.content.ends_with("Request: why did that fail"));
        assert!(!last.content.contains("sk-proj"));
        assert!(last.content.contains("exit status 1"));
        assert!(last.content.contains("boom"));
        assert!(m[0].content.contains("The last command failed"));
    }

    #[test]
    fn budget_drops_low_priority_first() {
        let mut c = ctx("kill something");
        c.processes = (0..300)
            .map(|i| Process {
                name: format!("proc{i}"),
                pid: i,
                cpu_percent: None,
                memory_mb: Some(1),
                ..Default::default()
            })
            .collect();
        let config = Config { max_context_tokens: Some(900), ..Config::default() };
        let m = build(&c, &config);
        let total: usize = m.iter().map(|m| estimate_tokens(&m.content)).sum();
        assert!(total <= 900 + 50, "{total}");
        let last = &m.last().unwrap().content;
        assert!(last.contains("Current folder: ~/a"));
        assert!(last.contains("proc0"));
        assert!(!last.contains("proc299"));
    }
}
