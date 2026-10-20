//! Turns a [`Context`] into chat messages for a small model.
//!
//! Everything bro knows is shown much as the person would have seen it in their terminal. Facts
//! about this Mac (apps, processes, tabs, tools, manual pages…) come first, each under a plain
//! label (`Open tabs:`), since a small model copies fake commands it sees. Then where the person
//! is, as commands with their output (`$ pwd`, `$ tree -F -L 2`, `$ git status -sb`), then the
//! commands they actually ran, with output and `zsh: exit N` after a failure, and last
//! `Request: fix it`, right after the command it's about. Earlier requests to bro in this
//! session are turns of their own: the request, then bro's answer.
//!
//! 1. instructions, with only the rules that apply to this request (`[tag]` lines in
//!    `prompts/system.md`);
//! 2. up to three built-in examples as earlier turns, each a short session of the same shape;
//! 3. the session. Each block has a priority; over the budget ([`Config::budget`]) the lowest
//!    go first, and what's left is shown in session order. Redaction runs last, on every message.
//!
//! The model answers in JSON: `{"command", "explanation", "confidence"}`.

use crate::config::Config;
use crate::context::{self, Context};
use serde::Deserialize;

const SYSTEM: &str = include_str!("../prompts/system.md");
const EXAMPLES: &str = include_str!("../prompts/examples.toml");
/// Examples shown. Apple's guidance for its on-device model is fewer than five.
const SHOWN_EXAMPLES: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
}

#[derive(Deserialize)]
struct Bank {
    example: Vec<BuiltIn>,
}

#[derive(Deserialize, Clone, Debug)]
struct BuiltIn {
    /// The session before the request, as `$ command` lines and output.
    #[serde(default)]
    session: String,
    /// Where the request was made (`~` if not given).
    #[serde(default)]
    folder: String,
    #[serde(default)]
    line: String,
    /// Text piped into the request, and the command it came from.
    #[serde(default)]
    piped: String,
    #[serde(default)]
    piped_command: String,
    /// An earlier request in the same session and bro's answer to it, shown as a turn of its own
    /// after `session`, as real earlier requests are.
    #[serde(default)]
    earlier_request: String,
    #[serde(default)]
    earlier_command: String,
    #[serde(default)]
    earlier_explanation: String,
    request: String,
    command: String,
    explanation: String,
    /// Kinds of situation it demonstrates: programs, tabs, processes, failed, line, piped.
    #[serde(default)]
    shapes: Vec<String>,
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

pub fn build(context: &Context, config: &Config) -> Vec<Message> {
    let words = context::request_words(&format!("{} {}", context.request, context.line));
    let mut messages = vec![Message { role: "system", content: system_prompt(config, context) }];
    for ex in pick_builtin(context, config) {
        let folder = if ex.folder.is_empty() { "~" } else { &ex.folder };
        // Machine facts under the same labels as in the real session.
        let mut session: String =
            ex.session.trim_end().lines().map(|l| label_for(l).unwrap_or_else(|| l.to_string()) + "\n").collect();
        if !ex.earlier_request.is_empty() {
            session.push_str(&format!("Request: {}", ex.earlier_request));
            messages.push(Message { role: "user", content: std::mem::take(&mut session) });
            messages.push(Message {
                role: "assistant",
                content: answer_text(&ex.earlier_command, &ex.earlier_explanation),
            });
        }
        let piped = (!ex.piped.trim().is_empty()).then(|| (ex.piped_command.as_str(), ex.piped.trim()));
        session.push_str(&request_text(folder, &ex.request, &ex.line, piped));
        messages.push(Message { role: "user", content: session });
        messages.push(Message { role: "assistant", content: answer_text(&ex.command, &ex.explanation) });
    }

    let last = last_block(context, config);
    let fixed: usize = messages.iter().map(|m| estimate_tokens(&m.content)).sum::<usize>() + 20;
    let budget = config.budget().saturating_sub(fixed);
    let mut blocks = blocks(context, &words, config);
    blocks.push(last);
    // Earlier requests answered in this session become turns of their own.
    let mut user = String::new();
    for (text, answer) in fit_parts(blocks, budget) {
        if !user.is_empty() {
            user.push('\n');
        }
        user.push_str(&text);
        if let Some(answer) = answer {
            messages.push(Message { role: "user", content: std::mem::take(&mut user) });
            messages.push(Message { role: "assistant", content: answer });
        }
    }
    messages.push(Message { role: "user", content: user });

    for m in &mut messages {
        m.content = redact::redact(&m.content).into_owned();
    }
    messages
}

/// Instructions with only the `[tag]` rules that apply.
fn system_prompt(config: &Config, c: &Context) -> String {
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
                    "failed" => last_failed(c),
                    "line" => !c.line.trim().is_empty(),
                    "tabs" => !c.tabs.is_empty(),
                    "manual" => !c.manuals.is_empty(),
                    "programs" => !c.programs.is_empty(),
                    "learned" => !c.examples.is_empty(),
                    "piped" => c.piped.is_some(),
                    "notes" => !c.notes.trim().is_empty(),
                    "asked" => {
                        !c.examples.is_empty()
                            || c.recent_commands.iter().any(|r| context::learned::asked(&r.command).is_some())
                    }
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

fn last_failed(c: &Context) -> bool {
    c.recent_commands
        .iter()
        .rev()
        .find(|r| context::learned::asked(&r.command).is_none())
        .and_then(|r| r.exit_status)
        .is_some_and(|s| s != 0)
}

/// The request, after the session: `Request: fix it`. Piped text comes first, as the command
/// that piped it and the text (`~/code $ cat app.log | bro`), and a half-typed command line
/// (Ctrl-G) as `Typed so far: tar -czf …`. `piped`: (source command, text).
fn request_text(folder: &str, request: &str, line: &str, piped: Option<(&str, &str)>) -> String {
    let mut s = String::new();
    if let Some((source, text)) = piped {
        s.push_str(&format!("{folder} $ {source} | bro\n{text}\n"));
    }
    if !line.trim().is_empty() {
        s.push_str(&format!("Typed so far: {}\n", line.trim()));
    }
    s.push_str(&format!("Request: {}", request.trim()));
    s
}

/// Kinds of situation an example demonstrates, so a request gets examples of the same shape
/// even when it shares no words with them ("open my sysadmin agent" ~ "start my backup tool").
fn shapes_of_request(c: &Context, words: &[String]) -> Vec<&'static str> {
    let mut v = Vec::new();
    if !c.programs.is_empty() {
        v.push("programs");
    }
    if !c.tabs.is_empty() {
        v.push("tabs");
    }
    if c.processes.iter().any(|p| context::relevant(&p.name, words)) {
        v.push("processes");
    }
    if last_failed(c) {
        v.push("failed");
    }
    if !c.line.trim().is_empty() {
        v.push("line");
    }
    if c.piped.is_some() {
        v.push("piped");
    }
    v
}

/// The most similar built-in examples (by shared words, and by the kinds of context present),
/// least similar first, so the best one sits right before the real request.
fn pick_builtin(c: &Context, config: &Config) -> Vec<BuiltIn> {
    let bank: Bank = toml::from_str(EXAMPLES).expect("prompts/examples.toml is valid");
    // Split file names too (`app.log` matches "log"), so examples match on what things are.
    let stems = |text: &str| -> Vec<String> {
        context::request_words(&text.replace(['.', '/'], " ")).iter().map(|w| context::stem(w)).collect()
    };
    let words = stems(&format!("{} {}", c.request, c.line));
    let shapes = shapes_of_request(c, &context::request_words(&format!("{} {}", c.request, c.line)));
    let mut scored: Vec<(usize, usize, BuiltIn)> = bank
        .example
        .into_iter()
        .enumerate()
        .map(|(i, ex)| {
            let theirs = stems(&format!("{} {} {}", ex.request, ex.line, ex.command));
            let overlap = words.iter().filter(|w| theirs.contains(w)).count();
            let same_shape = ex.shapes.iter().filter(|s| shapes.contains(&s.as_str())).count();
            (overlap + 2 * same_shape, i, ex)
        })
        .collect();
    // Best overlap first; the bank's own order breaks ties, so results are deterministic.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // Unrelated examples only prime the model (a `pkill` example invites killing), so none
    // rather than a weak one.
    let mut picked: Vec<BuiltIn> = scored
        .into_iter()
        .filter(|(score, _, _)| *score > 0)
        .take(SHOWN_EXAMPLES)
        .map(|(_, _, mut ex)| {
            ex.command = expand_search(&ex.command, config);
            ex
        })
        .collect();
    picked.reverse();
    picked
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

/// Size limits per source. With a small window (apfel) sources are trimmed hard (SPEC: the last
/// command's output at ~20 lines, the top 5 processes, 5 past requests).
struct Limits {
    output_lines: usize,
    /// Lines of text piped into this request: it was sent on purpose, so more of it is kept.
    piped_lines: usize,
    outputs_shown: usize,
    /// History lines shown (the newest).
    recent: usize,
    processes: usize,
    examples: usize,
    environment: usize,
    names: usize,
    apps: usize,
    projects: usize,
    notes: usize,
}

impl Limits {
    fn for_config(config: &Config) -> Limits {
        if config.small() {
            Limits {
                output_lines: 20,
                piped_lines: 60,
                outputs_shown: 1,
                recent: 10,
                processes: 5,
                examples: 5.min(config.history_examples),
                environment: 120,
                names: 60,
                apps: 120,
                projects: 20,
                notes: 400,
            }
        } else {
            Limits {
                output_lines: 60,
                piped_lines: 200,
                outputs_shown: 3,
                recent: usize::MAX,
                processes: 15,
                examples: config.history_examples,
                environment: 400,
                names: 200,
                apps: 400,
                projects: 60,
                notes: 1_500,
            }
        }
    }
}

/// A command and its output in the session. Lower `priority` is kept first when trimming.
struct Block {
    priority: u32,
    text: String,
    /// When shortened, keep the end of the output (a command's last lines say what happened)
    /// rather than its start (a listing's first entries).
    keep_tail: bool,
    /// An earlier request to bro ends here, and this was bro's answer (shown as an assistant
    /// turn).
    answer: Option<String>,
}

impl Block {
    fn new(priority: u32, text: String) -> Block {
        Block { priority, text, keep_tail: false, answer: None }
    }
}

// Priorities: what the request most likely refers to is kept longest.
const P_REQUEST: u32 = 0;
const P_LAST_COMMAND: u32 = 1;
const P_HERE: u32 = 2;
const P_NOTES: u32 = 3;
const P_LOOKUP: u32 = 4;
/// History lines before the newest get P_HISTORY + their distance from it.
const P_HISTORY: u32 = 10;
const P_TABS: u32 = 100;
const P_PROCESSES: u32 = 101;
const P_LEARNED: u32 = 102;
const P_ENVIRONMENT: u32 = 110;
const P_ALIASES: u32 = 111;
const P_APPS: u32 = 112;
const P_PROJECTS: u32 = 113;
const P_CONFIG: u32 = 114;
const P_SYSTEM: u32 = 115;

/// A command line in the session.
fn run(command: &str) -> String {
    format!("$ {command}")
}

/// `items` joined with `sep`, as many as fit in `max_tokens`, in order; one too big to fit is
/// skipped rather than ending the list.
fn capped(items: impl IntoIterator<Item = String>, sep: &str, max_tokens: usize) -> Option<String> {
    let mut out = String::new();
    for item in items {
        let piece = if out.is_empty() { item } else { format!("{sep}{item}") };
        if estimate_tokens(&out) + estimate_tokens(&piece) > max_tokens {
            continue;
        }
        out.push_str(&piece);
    }
    (!out.is_empty()).then_some(out)
}

/// The request itself, last: with piped text, `cat x | bro …` and the text (bro shows it, like
/// tee); with a half-typed line, that line.
fn last_block(c: &Context, config: &Config) -> Block {
    let folder = if c.folder.is_empty() { "~" } else { &c.folder };
    let lines = Limits::for_config(config).piped_lines;
    let piped = c.piped.as_ref().map(|p| (p.command.as_deref().unwrap_or("…"), head_and_tail(&p.output, lines)));
    let text = request_text(folder, &c.request, &c.line, piped.as_ref().map(|(s, t)| (*s, t.as_str())));
    Block::new(P_REQUEST, text)
}

/// Every source as a command and its output, in session order: what's always true of this Mac
/// first, then what this request looked up, then where the person is, then what they ran.
fn blocks(c: &Context, words: &[String], config: &Config) -> Vec<Block> {
    let limits = Limits::for_config(config);
    let here = (!c.folder.is_empty()).then_some(c.folder.as_str());
    let mut out = Vec::new();

    // Requests from earlier sessions, and what was run for them.
    if !c.examples.is_empty() {
        let mut s = String::new();
        for e in c.examples.iter().take(limits.examples).rev() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&format!("$ bro {}\n{}\n$ {}", e.request, e.command, e.command));
        }
        out.push(Block::new(P_LEARNED, s));
    }

    let sys = &c.system;
    if !sys.macos.is_empty() || !sys.arch.is_empty() {
        let mut s = run("sw_vers -productVersion; uname -m; echo $TERM_PROGRAM");
        for part in [&sys.macos, &sys.arch, &sys.terminal].into_iter().filter(|p| !p.is_empty()) {
            s.push('\n');
            s.push_str(part);
        }
        out.push(Block::new(P_SYSTEM, s));
    }
    if !sys.tools.is_empty() {
        let mut s = run(&format!("which {}", context::system::TOOLS.join(" ")));
        for t in context::system::TOOLS {
            match sys.tools.iter().find(|p| p.rsplit('/').next() == Some(t)) {
                Some(path) => s.push_str(&format!("\n{path}")),
                None => s.push_str(&format!("\n{t} not found")),
            }
        }
        out.push(Block::new(P_SYSTEM, s));
    }

    out.push(Block::new(P_CONFIG, {
        let mut s = run("cat ~/.config/bro/config.toml");
        s.push_str(&format!(
            "\nendpoint = \"{}\"\nmodel = \"{}\"\nsearch_url = \"{}\"\nbrowser = \"{}\"",
            config.endpoint, config.model, config.search_url, config.browser
        ));
        s
    }));

    // Named in the request first (the live index is already in this order; snapshots may not be).
    let by_relevance = |items: &[String]| -> Vec<String> {
        let mut v = items.to_vec();
        v.sort_by_key(|i| !context::relevant(i.rsplit('/').next().unwrap_or(i), words));
        v
    };
    if let Some(apps) = capped(by_relevance(&c.apps).into_iter().map(|a| format!("{a}.app")), ", ", limits.apps) {
        out.push(Block::new(P_APPS, format!("{}\n{apps}", run("ls -m /Applications"))));
    }

    // Home: projects and the programs in them, like `tree -F` (executables end in `*`). When
    // the person is at home, the folder listing joins in here rather than as a second tree.
    let mut home_paths: Vec<String> = by_relevance(&c.projects)
        .into_iter()
        .take(limits.projects)
        .filter_map(|p| p.strip_prefix("~/").map(|p| format!("{}/", p.trim_end_matches('/'))))
        .collect();
    home_paths.extend(c.programs.iter().filter_map(|p| p.strip_prefix("~/")).map(|p| format!("{p}*")));
    let at_home = c.folder == "~";
    if at_home {
        home_paths.extend(c.files.iter().cloned());
    }
    if !home_paths.is_empty() {
        let mut s = listing(c, "~", if at_home { 2 } else { 3 }, &home_paths);
        if at_home && c.files_total > c.files.iter().filter(|f| !f.trim_end_matches('/').contains('/')).count() {
            s.push_str(&format!("\n… {} entries in ~ in all", c.files_total));
        }
        out.push(Block::new(if at_home { P_HERE } else { P_PROJECTS }, s));
    }

    // Not all of `env`: the variables whose values may be shown, and others only when the
    // request names them (a long list of names is mostly noise). The command says it's a
    // selection, so the model doesn't take it for everything that's set.
    let mut env: Vec<&(String, Option<String>)> =
        c.environment.iter().filter(|(name, value)| value.is_some() || context::relevant(name, words)).collect();
    // Named in the request first, then the ones that most often decide a command.
    const FIRST: &[&str] = &["PATH", "SHELL", "EDITOR", "TERM_PROGRAM", "USER", "HOME", "LANG", "VIRTUAL_ENV"];
    env.sort_by_key(|(name, _)| {
        (!context::relevant(name, words), FIRST.iter().position(|f| f == name).unwrap_or(FIRST.len()))
    });
    let shown: Vec<String> = env
        .iter()
        .map(|(name, value)| match value {
            Some(v) if name.ends_with("PATH") && v.contains(':') => format!("{name}={}", dedupe_path(v)),
            Some(v) => format!("{name}={v}"),
            None => format!("{name}=[redacted]"),
        })
        .collect();
    if let Some(s) = capped(shown, "\n", limits.environment) {
        let names: Vec<&str> = s.lines().filter_map(|l| l.split('=').next()).collect();
        let head = format!("env | grep -E '^({})='", names.join("|"));
        out.push(Block::new(P_ENVIRONMENT, format!("{}\n{s}", run(&head))));
    }
    if let Some(e) = &c.active_env {
        let mut s = format!("{}\n{}/bin/python", run("which python"), e.prefix);
        if !e.bin.is_empty() {
            let mut bin: Vec<&String> = e.bin.iter().collect();
            bin.sort_by_key(|b| !context::relevant(b, words));
            if let Some(list) = capped(bin.into_iter().cloned(), "  ", limits.names) {
                s.push_str(&format!("\n{}\n{list}", run(&format!("ls ${}/bin", e.variable))));
            }
        }
        out.push(Block::new(P_HERE, s));
    }

    let mut aliases: Vec<&String> = c.aliases.iter().collect();
    aliases.sort_by_key(|a| !context::relevant(a.split('=').next().unwrap_or(a), words));
    if let Some(s) = capped(
        aliases.into_iter().map(|a| match a.split_once('=') {
            Some((name, value)) => format!("{name}='{}'", value.replace('\'', "'\\''")),
            None => a.clone(),
        }),
        "\n",
        limits.names,
    ) {
        out.push(Block::new(P_ALIASES, format!("{}\n{s}", run("alias"))));
    }
    let mut functions: Vec<&String> = c.functions.iter().collect();
    functions.sort_by_key(|f| !context::relevant(f, words));
    if let Some(s) = capped(functions.into_iter().cloned(), " ", limits.names / 2) {
        out.push(Block::new(P_ALIASES + 1, format!("{}\n{s}", run("echo ${(k)functions}"))));
    }

    // Processes, as `ps` sorted by CPU (-r) and by memory (-m). CPU is each one's average since
    // it started; RSS in kilobytes, as ps shows it.
    let table = |procs: Vec<&context::Process>| -> String {
        let mut s = String::from("  PID  %CPU      RSS COMM");
        for p in procs {
            s.push_str(&format!(
                "\n{:>5} {:>5.1} {:>8} {}",
                p.pid,
                p.cpu_percent.unwrap_or(0.0),
                p.memory_mb.unwrap_or(0) * 1024,
                p.name
            ));
        }
        s
    };
    let mut by_cpu: Vec<&context::Process> =
        c.processes.iter().filter(|p| p.cpu_percent.is_some_and(|c| c >= 1.0)).collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal));
    by_cpu.truncate(limits.processes);
    if !by_cpu.is_empty() {
        let head = format!("ps -Ao pid,%cpu,rss,comm -r | head -n {}", by_cpu.len() + 1);
        out.push(Block::new(P_PROCESSES, format!("{}\n{}", run(&head), table(by_cpu))));
    }
    let mut by_memory: Vec<&context::Process> = c.processes.iter().filter(|p| p.memory_mb.is_some()).collect();
    by_memory.sort_by_key(|p| std::cmp::Reverse(p.memory_mb));
    by_memory.truncate(limits.processes);
    if !by_memory.is_empty() {
        let head = format!("ps -Ao pid,%cpu,rss,comm -m | head -n {}", by_memory.len() + 1);
        out.push(Block::new(P_PROCESSES, format!("{}\n{}", run(&head), table(by_memory))));
    }
    // Processes named in the request, the way you'd look them up.
    for w in words {
        let found: Vec<&context::Process> =
            c.processes.iter().filter(|p| context::relevant(&p.name, std::slice::from_ref(w))).collect();
        if found.is_empty() {
            continue;
        }
        let mut s = run(&format!("pgrep -il {w}"));
        for p in found {
            s.push_str(&format!("\n{} {}", p.pid, p.name));
        }
        out.push(Block::new(P_LOOKUP, s));
    }
    if !c.jobs.is_empty() {
        let mut s = run("jobs");
        for (i, j) in c.jobs.iter().enumerate() {
            s.push_str(&format!("\n[{}]  {j}", i + 1));
        }
        out.push(Block::new(P_HERE, s));
    }

    if !c.tabs.is_empty() {
        let mut s = run("ctx list tabs");
        for t in &c.tabs {
            let name = if t.name.is_empty() { String::new() } else { format!(" \"{}\"", t.name) };
            let state = match &t.running {
                Some(r) => format!("busy: {r}"),
                None => "idle".into(),
            };
            s.push_str(&format!("\n  tab {}{name} at {} — {state}", t.number, tilde(&t.folder)));
        }
        out.push(Block::new(P_TABS, s));
    }
    if !c.saved_tab_commands.is_empty() {
        let query = context::ctx::find_query(words).unwrap_or_default();
        let mut s = run(&format!("ctx find \"{query}\""));
        for command in &c.saved_tab_commands {
            s.push_str(&format!("\n  {command}"));
        }
        out.push(Block::new(P_LOOKUP, s));
    }

    if let Some(tldr) = c.tldr.as_ref().filter(|t| !t.is_empty()) {
        let mut pages: Vec<&str> = Vec::new();
        for e in tldr {
            if !pages.contains(&e.page.as_str()) {
                pages.push(&e.page);
            }
        }
        for page in pages {
            let mut s = run(&format!("tldr {page}"));
            for e in tldr.iter().filter(|e| e.page == page) {
                s.push_str(&format!("\n- {}:\n    {}", e.description.trim_end_matches(':'), e.command));
            }
            out.push(Block::new(P_LOOKUP, s));
        }
    }
    for m in &c.manuals {
        out.push(Block::new(P_LOOKUP, format!("{}\n{}", run(&format!("man {}", m.command)), m.text.trim_end())));
    }

    if let Some(notes) = notes(c, limits.notes) {
        out.push(Block::new(P_NOTES, format!("{}\n{notes}", run("cat ~/.config/bro/bro.md"))));
    }

    // Where the person is now.
    if let Some(folder) = here.filter(|_| !at_home) {
        let mut s = format!("{}\n{folder}\n", run("pwd"));
        if c.files.is_empty() {
            s.push_str(&format!("{}\n.\n\n0 directories, 0 files", run("tree -F -L 2")));
        } else {
            s.push_str(&listing(c, ".", 2, &c.files));
            let top = c.files.iter().filter(|f| !f.trim_end_matches('/').contains('/')).count();
            if c.files_total > top {
                s.push_str(&format!("\n… {} more", c.files_total - top));
            }
        }
        out.push(Block::new(P_HERE, s));
    }
    if let (Some(_), Some(git)) = (here, &c.git) {
        let mut s = format!("{}\n## {}", run("git status -sb"), git.branch);
        if git.changes.is_empty() {
            if let Some(status) = git.status.as_deref().filter(|s| *s != "clean") {
                s.push_str(&format!(" [{status}]"));
            }
        } else {
            for change in &git.changes {
                s.push('\n');
                s.push_str(change);
            }
        }
        out.push(Block::new(P_HERE, s));
    }

    for b in &mut out {
        if let Some((first, rest)) = b.text.split_once('\n')
            && let Some(label) = label_for(first)
        {
            b.text = format!("{label}\n{rest}");
        }
    }
    out.extend(history(c, &limits));
    out
}

/// Machine facts go under a plain label instead of the command that would show them: a small
/// model copies a `$ ps …` line it sees into its answer.
fn label_for(command_line: &str) -> Option<String> {
    let c = command_line.strip_prefix("$ ")?;
    let after = |p: &str| c.strip_prefix(p).map(str::trim);
    Some(if c.starts_with("sw_vers") {
        "This Mac (macOS version, CPU, terminal):".into()
    } else if c == "which python" {
        "Active Python environment:".into()
    } else if c.starts_with("which ") {
        "Tools installed here (and not):".into()
    } else if c.starts_with("cat ~/.config/bro/config.toml") {
        "bro's settings (~/.config/bro/config.toml):".into()
    } else if c.starts_with("cat ~/.config/bro/bro.md") {
        "This person's notes for you (~/.config/bro/bro.md):".into()
    } else if c.starts_with("ls -m /Applications") {
        "Apps:".into()
    } else if c.starts_with("tree -F -L 3 ~") || c.starts_with("find ~") {
        "Folders and programs in the home folder (~):".into()
    } else if c.starts_with("env ") {
        "Some environment variables:".into()
    } else if c.starts_with("ls $") {
        "Programs in the active environment:".into()
    } else if c == "alias" {
        "Aliases:".into()
    } else if c.starts_with("echo ${(k)functions}") {
        "Shell functions:".into()
    } else if c.contains("-r | head") && c.starts_with("ps ") {
        "Running processes, busiest first (for reference; act on one only if asked):".into()
    } else if c.contains("-m | head") && c.starts_with("ps ") {
        "Running processes, largest first (for reference; act on one only if asked):".into()
    } else if let Some(w) = after("pgrep -il") {
        format!("Running processes named like \"{w}\":")
    } else if c == "jobs" {
        "Shell jobs:".into()
    } else if c == "ctx list tabs" {
        "Open tabs:".into()
    } else if c.starts_with("ctx find") {
        "Commands from saved tabs:".into()
    } else if let Some(p) = after("tldr") {
        format!("Examples from the tldr page for {p}:")
    } else {
        format!("Manual excerpt for {} on this Mac:", after("man ")?)
    })
}

/// The commands the person ran, oldest first, one block each: the newest with the highest
/// priority. Output where known, `zsh: exit N` after a failure (as zsh's `print_exit_value`
/// shows it), and after an earlier `bro …` request, what bro answered.
fn history(c: &Context, limits: &Limits) -> Vec<Block> {
    let mut out = Vec::new();
    let mut piped_used = vec![false; c.earlier_piped.len()];
    let skip = c.recent_commands.len().saturating_sub(limits.recent);
    let shown = &c.recent_commands[skip..];
    let first_output = shown.len().saturating_sub(limits.outputs_shown);
    let mut blocks = Vec::new();
    for (i, r) in shown.iter().enumerate() {
        let mut s = run(&r.command);
        let mut keep_tail = false;
        let mut answer = None;
        if let (Some(request), Some(suggested)) = (context::learned::asked(&r.command), &r.suggested) {
            s = format!("Request: {request}");
            answer = Some(answer_text(suggested, r.suggested_explanation.as_deref().unwrap_or("")));
        } else if let Some(suggested) = &r.suggested {
            s.push('\n');
            s.push_str(suggested);
        }
        // `cat x | bro …` earlier in this terminal: what was piped in, as bro showed it.
        let piped = c.earlier_piped.iter().enumerate().find(|(j, p)| {
            !piped_used[*j] && p.command.as_deref().is_some_and(|src| is_piped_into_bro(&r.command, src))
        });
        if let Some((j, p)) = piped {
            piped_used[j] = true;
            s.push('\n');
            s.push_str(&context::ctx::tail_lines(&p.output, limits.output_lines));
            keep_tail = true;
        } else if i >= first_output
            && let Some(output) = r.output.as_deref().filter(|o| !o.trim().is_empty())
        {
            s.push('\n');
            s.push_str(&context::ctx::tail_lines(output, limits.output_lines));
            keep_tail = true;
        }
        if let Some(status) = r.exit_status.filter(|s| *s != 0) {
            s.push_str(&format!("\nzsh: exit {status}"));
        }
        let age = (shown.len() - 1 - i) as u32;
        let priority = if age == 0 { P_LAST_COMMAND } else { P_HISTORY + age };
        blocks.push(Block { priority, text: s, keep_tail, answer });
    }
    // Piped text whose command has scrolled out of the history shown: just before it.
    for (p, used) in c.earlier_piped.iter().zip(&piped_used) {
        if !used {
            let mut s = run(&format!("{} | bro", p.command.as_deref().unwrap_or("…")));
            s.push('\n');
            s.push_str(&context::ctx::tail_lines(&p.output, limits.output_lines));
            out.push(Block { priority: P_HISTORY + 50, text: s, keep_tail: true, answer: None });
        }
    }
    out.extend(blocks);
    out
}

/// A `:`-separated path list with repeats left out (first one kept), as `typeset -U path` would.
fn dedupe_path(value: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for part in value.split(':') {
        if !part.is_empty() && !seen.contains(&part) {
            seen.push(part);
        }
    }
    seen.join(":")
}

/// True if `line` is `<source> | bro …`.
fn is_piped_into_bro(line: &str, source: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix(source.trim()) else { return false };
    let rest = rest.trim_start();
    rest.strip_prefix('|').is_some_and(|r| r.trim_start().to_lowercase().starts_with("bro"))
}

/// bro.md, cut to whole lines within its share of the budget.
fn notes(c: &Context, max_tokens: usize) -> Option<String> {
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

fn tilde(path: &str) -> String {
    context::folder::tilde(std::path::Path::new(path))
}

/// A folder view: `tree` if it's installed (or unknown, in an old snapshot), else `find`,
/// so the session only shows commands that work here. `depth` as in `tree -L`.
fn listing(c: &Context, root: &str, depth: usize, paths: &[String]) -> String {
    let tree_here = c.system.tools.is_empty() || c.system.tools.iter().any(|t| t.ends_with("/tree"));
    if tree_here {
        let arg = if root == "." { String::new() } else { format!(" {root}") };
        return format!("{}\n{}", run(&format!("tree -F -L {depth}{arg}")), tree(root, paths));
    }
    let mut s = run(&format!("find {root} -maxdepth {depth} -not -path '*/.*'"));
    s.push_str(&format!("\n{root}"));
    for p in paths {
        if p.contains("… ") {
            continue;
        }
        s.push_str(&format!("\n{root}/{}", p.trim_end_matches(['/', '*'])));
    }
    s
}

/// Paths (relative to `root`, folders ending in `/`) drawn as `tree` draws them. Folders that
/// are only on the way to a listed path are drawn too. Siblings are sorted like `tree`.
fn tree(root: &str, paths: &[String]) -> String {
    #[derive(Default)]
    struct Node {
        children: std::collections::BTreeMap<String, Node>,
    }
    let mut top = Node::default();
    for path in paths {
        // "… 12 more" entries are leaves whose name holds a slash-free note.
        let (dir, leaf) = match path.find("… ") {
            Some(at) => (&path[..at], Some(&path[at..])),
            None => (path.as_str(), None),
        };
        let mut node = &mut top;
        let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let is_last = i + 1 == parts.len() && leaf.is_none();
            let is_dir = !is_last || dir.ends_with('/');
            let key = if is_dir { format!("{part}/") } else { (*part).to_string() };
            node = node.children.entry(key).or_default();
        }
        if let Some(leaf) = leaf {
            node.children.entry(format!("\u{10FFFF}{leaf}")).or_default();
        }
    }
    // A folder whose only entry is another folder is drawn on one line with it
    // (`Workspaces/charles-terminal-experience/bro/`), so a deep path reads whole.
    fn draw(node: &Node, prefix: &str, out: &mut String) {
        let n = node.children.len();
        for (i, (name, child)) in node.children.iter().enumerate() {
            let last = i + 1 == n;
            let mut name = name.trim_start_matches('\u{10FFFF}').to_string();
            let mut child = child;
            while name.ends_with('/')
                && child.children.len() == 1
                && let Some((only, grandchild)) = child.children.iter().next()
                && only.ends_with('/')
            {
                name.push_str(only);
                child = grandchild;
            }
            out.push_str(&format!("\n{prefix}{}{name}", if last { "└── " } else { "├── " }));
            draw(child, &format!("{prefix}{}", if last { "    " } else { "│   " }), out);
        }
    }
    let mut out = root.to_string();
    draw(&top, "", &mut out);
    out
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

/// Keep blocks by priority while they fit; a block that doesn't fit is shortened to its command
/// line and whole lines of output if that leaves some output, otherwise dropped. Kept blocks are
/// shown in session order.
#[cfg(test)]
fn fit(blocks: Vec<Block>, budget: usize) -> String {
    fit_parts(blocks, budget).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n")
}

/// Kept blocks in session order, each with the answer that followed it, if any.
fn fit_parts(blocks: Vec<Block>, budget: usize) -> Vec<(String, Option<String>)> {
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by_key(|&i| (blocks[i].priority, std::cmp::Reverse(i)));
    let mut used = 0;
    let mut kept: Vec<Option<String>> = vec![None; blocks.len()];
    for i in order {
        let b = &blocks[i];
        let cost = estimate_tokens(&b.text) + 1;
        if used + cost <= budget || b.priority == P_REQUEST {
            used += cost;
            kept[i] = Some(b.text.clone());
            continue;
        }
        let mut lines = b.text.lines();
        let Some(command) = lines.next() else { continue };
        let rest: Vec<&str> = lines.collect();
        let mut room = budget.saturating_sub(used + estimate_tokens(command) + 2);
        let mut taken: Vec<&str> = Vec::new();
        let candidates: Box<dyn Iterator<Item = &&str>> =
            if b.keep_tail { Box::new(rest.iter().rev()) } else { Box::new(rest.iter()) };
        for l in candidates {
            let t = estimate_tokens(l);
            if t > room {
                break;
            }
            room -= t;
            taken.push(l);
        }
        if taken.is_empty() {
            continue;
        }
        if b.keep_tail {
            taken.reverse();
        }
        let skipped = rest.len() - taken.len();
        let note = format!("… ({skipped} lines not shown)");
        let text = if b.keep_tail {
            format!("{command}\n{note}\n{}", taken.join("\n"))
        } else {
            format!("{command}\n{}\n{note}", taken.join("\n"))
        };
        used += estimate_tokens(&text) + 1;
        kept[i] = Some(text);
    }
    kept.into_iter().zip(blocks).filter_map(|(k, b)| k.map(|t| (t, b.answer))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Process, RecentCommand, Tab};

    fn ctx(request: &str) -> Context {
        Context { request: request.into(), folder: "~/a".into(), ..Context::default() }
    }

    fn session(c: &Context, config: &Config) -> String {
        build(c, config).pop().unwrap().content
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
    fn examples_are_relevant_and_most_similar_last() {
        let mut c = ctx("kill that ghostty process");
        c.processes = vec![Process {
            name: "ghostty".into(),
            pid: 812,
            cpu_percent: Some(1.0),
            memory_mb: Some(600),
            ..Default::default()
        }];
        let ex = pick_builtin(&c, &Config::default());
        assert!(!ex.is_empty());
        assert!(ex.last().unwrap().command.starts_with("pkill"), "most similar example last: {ex:?}");
        let m = build(&c, &Config::default());
        // Examples are sessions of the same shape: labelled facts, then the request.
        let slack = m.iter().find(|m| m.content.contains("Slack")).expect("the process example is shown");
        assert!(
            slack
                .content
                .starts_with("Running processes, largest first (for reference; act on one only if asked):\n  PID"),
            "{}",
            slack.content
        );
        assert!(slack.content.ends_with("\nRequest: close slack it's frozen"), "{}", slack.content);
    }

    #[test]
    fn an_example_with_an_earlier_request_shows_it_as_a_turn() {
        let mut c = ctx("bump the image tag to 2.1");
        c.piped = Some(crate::context::Piped {
            command: Some("cat deploy.yaml".into()),
            output: "image: x:2.0".into(),
            time: 0,
        });
        let m = build(&c, &Config::default());
        let i = m
            .iter()
            .position(|m| m.content == "Request: bump the image tag in deploy.yaml to 2.1")
            .expect("earlier request");
        assert_eq!(m[i + 1].role, "assistant");
        assert!(m[i + 1].content.contains("cat deploy.yaml | bro bump the image tag to 2.1"), "{}", m[i + 1].content);
        assert!(m[i + 2].content.starts_with("~/infra $ cat deploy.yaml | bro\nservices:"), "{}", m[i + 2].content);
    }

    #[test]
    fn the_request_follows_the_command_it_is_about() {
        let mut c = ctx("why did that fail, fix it");
        c.recent_commands = vec![
            RecentCommand { command: "cd ~/a".into(), ..Default::default() },
            RecentCommand {
                command: "cargo build".into(),
                exit_status: Some(101),
                output: Some("error[E0432]: unresolved import `serde`".into()),
                suggested: None,
                suggested_explanation: None,
            },
        ];
        c.processes = vec![Process {
            name: "node".into(),
            pid: 7,
            cpu_percent: Some(3.0),
            memory_mb: Some(9),
            ..Default::default()
        }];
        c.files = vec!["Cargo.toml".into(), "src/".into(), "src/main.rs".into()];
        c.files_total = 2;
        let s = session(&c, &Config::default());
        assert!(
            s.ends_with("$ cargo build\nerror[E0432]: unresolved import `serde`\nzsh: exit 101\nRequest: why did that fail, fix it"),
            "{s}"
        );
        assert!(s.contains("$ pwd\n~/a\n$ tree -F -L 2\n.\n├── Cargo.toml\n└── src/\n    └── main.rs"), "{s}");
        assert!(
            s.find("Running processes").unwrap() < s.find("$ cd ~/a").unwrap(),
            "machine facts come before the history"
        );
        let system = &build(&c, &Config::default())[0].content;
        assert!(system.contains("failed"), "{system}");
    }

    #[test]
    fn earlier_requests_show_what_bro_answered() {
        let mut c = ctx("no, show hidden files too");
        c.recent_commands = vec![
            RecentCommand {
                command: "bro list the files".into(),
                suggested: Some("ls".into()),
                suggested_explanation: Some("Lists the files here.".into()),
                ..Default::default()
            },
            RecentCommand { command: "pwd".into(), exit_status: Some(0), ..Default::default() },
        ];
        let m = build(&c, &Config::default());
        let n = m.len();
        // The earlier request and bro's answer are a turn of their own, then what was run since.
        assert!(m[n - 3].content.ends_with("\nRequest: list the files"), "{}", m[n - 3].content);
        assert_eq!(m[n - 2].role, "assistant");
        assert_eq!(m[n - 2].content, answer_text("ls", "Lists the files here."));
        assert_eq!(m[n - 1].content, "$ pwd\nRequest: no, show hidden files too");
        assert!(m[0].content.contains("Earlier requests are what they asked you before"));
        // bro's own subcommands aren't requests.
        c.recent_commands[0].command = "bro config".into();
        assert!(session(&c, &Config::default()).contains("$ bro config\nls\n$ pwd"));
    }

    #[test]
    fn rules_only_when_they_apply() {
        let config = Config::default();
        let plain = build(&ctx("list files"), &config);
        // Rules about kinds of context appear only when that context is there; never by keyword.
        assert!(!plain[0].content.contains("ctx show"));
        let mut c = ctx("show me the dev server tab");
        c.tabs = vec![Tab {
            number: 42,
            name: "Dev server".into(),
            folder: "/Users/x/site".into(),
            running: Some("npm run dev".into()),
        }];
        let m = build(&c, &config);
        assert!(m[0].content.contains("ctx show"));
        assert!(
            m.last()
                .unwrap()
                .content
                .contains("Open tabs:\n  tab 42 \"Dev server\" at /Users/x/site — busy: npm run dev")
        );
    }

    #[test]
    fn piped_text_comes_just_before_the_request() {
        let mut c = ctx("set the web search to google");
        c.piped = Some(crate::context::Piped {
            command: Some("cat ~/.config/bro/config.toml".into()),
            output: (1..=300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
            time: 0,
        });
        let m = build(&c, &Config::default());
        let user = &m.last().unwrap().content;
        assert!(user.contains("~/a $ cat ~/.config/bro/config.toml | bro\nline 1\n"), "{user}");
        assert!(
            user.ends_with("line 300\nRequest: set the web search to google") && user.contains(" lines skipped)"),
            "{user}"
        );
        assert!(m[0].content.contains("Don't pipe it into bro again"));
        assert!(!build(&ctx("change your web search"), &Config::default())[0].content.contains("Don't pipe it"));
    }

    #[test]
    fn a_half_typed_line_comes_before_the_request() {
        let mut c = ctx("finish or fix this command line");
        c.line = "tar -czf out.tgz".into();
        let m = build(&c, &Config::default());
        assert!(
            m.last()
                .unwrap()
                .content
                .ends_with("Typed so far: tar -czf out.tgz\nRequest: finish or fix this command line")
        );
        assert!(m[0].content.contains("\"Typed so far\""));
    }

    #[test]
    fn earlier_piped_text_sits_under_its_command() {
        let mut c = ctx("now change the model too");
        c.recent_commands = vec![RecentCommand {
            command: "cat ~/.config/bro/config.toml | bro set the web search to google".into(),
            suggested: Some("sed -i '' 's|search_url = .*|…|' ~/.config/bro/config.toml".into()),
            ..Default::default()
        }];
        c.earlier_piped = vec![crate::context::Piped {
            command: Some("cat ~/.config/bro/config.toml".into()),
            output: "model = \"x\"".into(),
            time: 0,
        }];
        let s = session(&c, &Config::default());
        assert!(s.contains("| bro set the web search to google\nsed -i"), "{s}");
        assert!(s.ends_with("config.toml\nmodel = \"x\"\nRequest: now change the model too"), "{s}");
    }

    #[test]
    fn notes_from_bro_md_within_their_budget() {
        let mut c = ctx("connect to the gpu box");
        c.notes = "My GPU box is `ssh lab-gpu`.\n".to_string() + &"- filler line\n".repeat(2000);
        let m = build(&c, &Config::default());
        let s = m.last().unwrap().content.clone();
        assert!(s.contains("This person's notes for you (~/.config/bro/bro.md):\nMy GPU box is `ssh lab-gpu`."), "{s}");
        assert!(estimate_tokens(&s) < 2_500, "bro.md is capped");
        assert!(m[0].content.contains("bro.md"));
    }

    #[test]
    fn every_source_is_shown() {
        let mut c = ctx("zip the src folder");
        c.processes = vec![Process {
            name: "node".into(),
            pid: 1,
            cpu_percent: Some(2.0),
            memory_mb: Some(1),
            ..Default::default()
        }];
        c.environment = vec![("EDITOR".into(), Some("vim".into())), ("GITHUB_TOKEN".into(), None)];
        c.aliases = vec!["ll=ls -l".into(), "gs".into()];
        c.apps = vec!["Safari".into()];
        c.projects = vec!["~/code/site".into()];
        c.programs = vec!["~/code/site/bin/deploy".into()];
        c.git = Some(crate::context::Git {
            branch: "main".into(),
            status: Some("1 modified".into()),
            changes: vec![" M x.rs".into()],
        });
        c.jobs = vec!["vim notes.md".into()];
        c.system = context::system::System { macos: "26.1".into(), arch: "arm64".into(), ..Default::default() };
        let s = session(&c, &Config::default());
        // Facts about the Mac under plain labels; where the person is, as the commands that show it.
        for expected in [
            "Running processes, busiest first (for reference; act on one only if asked):\n  PID  %CPU      RSS COMM\n    1   2.0     1024 node",
            "Some environment variables:\nEDITOR=vim\n",
            "Aliases:\nll='ls -l'\ngs",
            "Apps:\nSafari.app",
            "Folders and programs in the home folder (~):\n~\n└── code/site/bin/\n    └── deploy*",
            "Shell jobs:\n[1]  vim notes.md",
            "This Mac (macOS version, CPU, terminal):\n26.1\narm64",
            "bro's settings (~/.config/bro/config.toml):\nendpoint = ",
            "$ pwd\n~/a\n",
            "$ git status -sb\n## main\n M x.rs",
        ] {
            assert!(s.contains(expected), "{expected} missing from:\n{s}");
        }
        // A variable without a value to show is noise unless the request is about it.
        assert!(!s.contains("GITHUB_TOKEN"), "{s}");
        c.request = "is my github token set".into();
        assert!(session(&c, &Config::default()).contains("GITHUB_TOKEN=[redacted]"));
    }

    #[test]
    fn small_windows_keep_the_top_five_processes_plus_named_ones() {
        let mut c = ctx("kill ghostty");
        c.processes = (0..20)
            .map(|i| Process {
                name: format!("proc{i}"),
                pid: i,
                cpu_percent: Some(50.0 - i as f32),
                memory_mb: Some(1),
                ..Default::default()
            })
            .collect();
        c.processes.push(Process { name: "ghostty".into(), pid: 99, ..Default::default() });
        let s = session(&c, &Config::default());
        assert!(s.contains("Running processes named like \"ghostty\":\n99 ghostty"), "{s}");
        assert!(s.contains("proc4\n") && !s.contains("proc5\n"), "apfel: the top 5 by cpu: {s}");
        let big = Config { model: "qwen3-8b".into(), ..Config::default() };
        assert!(session(&c, &big).contains("proc14\n"), "6,000 tokens: the top 15");
    }

    #[test]
    fn request_comes_last_and_is_redacted() {
        let mut c = ctx("why did that fail");
        c.recent_commands = vec![RecentCommand {
            command: "export OPENAI_API_KEY=sk-proj-Qk3vT9xLm2Rp8WzA4bN6cY1dF7gH0jKs".into(),
            exit_status: Some(1),
            output: Some("boom".into()),
            suggested: None,
            suggested_explanation: None,
        }];
        let m = build(&c, &Config::default());
        let last = m.last().unwrap();
        assert!(last.content.ends_with("boom\nzsh: exit 1\nRequest: why did that fail"), "{}", last.content);
        assert!(!last.content.contains("sk-proj"));
    }

    #[test]
    fn budget_drops_low_priority_first() {
        let mut c = ctx("kill something");
        c.apps = (0..300).map(|i| format!("App number {i}")).collect();
        c.environment = (0..300).map(|i| (format!("VAR_{i}"), Some("x".repeat(20)))).collect();
        c.recent_commands = vec![RecentCommand {
            command: "make".into(),
            exit_status: Some(2),
            output: Some("x\n".repeat(40)),
            suggested: None,
            suggested_explanation: None,
        }];
        // The instructions and examples take about 800 of these; 300 apps and variables can't fit.
        let config = Config { max_context_tokens: Some(1_300), ..Config::default() };
        let m = build(&c, &config);
        let total: usize = m.iter().map(|m| estimate_tokens(&m.content)).sum();
        assert!(total <= 1_300 + 60, "{total}");
        let last = &m.last().unwrap().content;
        assert!(last.contains("$ make\n"), "{last}");
        assert!(!last.contains("App number 299") && !last.contains("VAR_299"), "{last}");
        assert!(last.ends_with("zsh: exit 2\nRequest: kill something"), "{last}");
    }

    #[test]
    fn shortened_output_keeps_its_end() {
        let blocks = vec![
            Block {
                priority: P_LAST_COMMAND,
                text: format!("$ make\n{}\nthe error", "noise line\n".repeat(200)),
                keep_tail: true,
                answer: None,
            },
            Block::new(P_REQUEST, "Request: fix it".into()),
        ];
        let s = fit(blocks, 120);
        assert!(s.starts_with("$ make\n… ("), "{s}");
        assert!(s.ends_with("the error\nRequest: fix it"), "{s}");
    }

    #[test]
    fn deep_single_paths_read_whole() {
        assert_eq!(
            tree("~", &["Workspaces/charles-terminal-experience/bro/".into(), "code/site/".into(), "code/api/".into()]),
            "~\n├── Workspaces/charles-terminal-experience/bro/\n└── code/\n    ├── api/\n    └── site/"
        );
    }

    #[test]
    fn tree_like_tree() {
        assert_eq!(
            tree(".", &["a/".into(), "a/x".into(), "a/… 3 more".into(), "b.txt".into()]),
            ".\n├── a/\n│   ├── x\n│   └── … 3 more\n└── b.txt"
        );
    }
}
