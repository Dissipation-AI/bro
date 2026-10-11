//! Everything bro knows when it asks the model. Each source runs on its own thread with a time
//! limit; a slow or missing source is skipped, never waited on. Missing context is normal.
//!
//! The same struct is the eval snapshot format (`--context-file`), so evals replay exactly what a
//! live request would have seen.

pub mod ctx;
pub mod folder;
pub mod history;
pub mod index;
pub mod learned;
pub mod man;
pub mod piped;
pub mod processes;
pub mod system;
pub mod tldr;

use crate::config::Config;
use crate::timing::Timings;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Context {
    /// What the person asked for.
    pub request: String,
    /// A half-typed command line, when bro was started with Ctrl-G.
    pub line: String,
    /// This person's instructions for bro on this computer (`~/.config/bro/bro.md`).
    pub notes: String,
    /// Output piped into this request (`cat file | bro …`).
    pub piped: Option<Piped>,
    /// Output piped into bro earlier in this terminal session, oldest first.
    pub earlier_piped: Vec<Piped>,
    pub folder: String,
    pub git: Option<Git>,
    /// First entries of the folder listing; directories end in `/`.
    pub files: Vec<String>,
    pub files_total: usize,
    /// Oldest first.
    pub recent_commands: Vec<RecentCommand>,
    pub tabs: Vec<Tab>,
    pub processes: Vec<Process>,
    pub jobs: Vec<String>,
    /// Name and, where allowed, value.
    pub environment: Vec<(String, Option<String>)>,
    pub aliases: Vec<String>,
    pub functions: Vec<String>,
    pub apps: Vec<String>,
    pub default_browser: Option<String>,
    pub projects: Vec<String>,
    /// Executables inside matching projects (their folder and `bin/`).
    pub programs: Vec<String>,
    pub examples: Vec<learned::Example>,
    pub saved_tab_commands: Vec<String>,
    pub manuals: Vec<man::Manual>,
    /// Matching tldr examples for installed commands. `None` in a snapshot means "look them up".
    pub tldr: Option<Vec<tldr::Example>>,
    pub system: system::System,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Git {
    pub branch: String,
    /// e.g. "2 staged, 3 modified, 1 untracked", or "clean".
    pub status: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentCommand {
    pub command: String,
    pub exit_status: Option<i32>,
    /// Tail of the output, when known (inside ctx, or in an eval snapshot).
    pub output: Option<String>,
    /// For an earlier request left in the history as `# bro …`: what bro suggested then.
    pub suggested: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Piped {
    /// The command whose output it is, when known.
    pub command: Option<String>,
    pub output: String,
    /// Unix seconds, for expiring saved output.
    pub time: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    pub number: u64,
    pub name: String,
    pub folder: String,
    pub running: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Process {
    pub name: String,
    pub pid: i32,
    /// Percent of one core; `None` for other users' processes.
    pub cpu_percent: Option<f32>,
    pub memory_mb: Option<u64>,
    /// `cpu_percent` is the average since the process started, not a measurement of right now.
    pub cpu_average: bool,
}

/// Inputs the zsh widget passes in, which bro can't read itself.
#[derive(Debug, Default)]
pub struct ShellState {
    pub history: Option<Vec<String>>,
    pub last_status: Option<i32>,
    pub aliases: Vec<String>,
    pub functions: Vec<String>,
    pub jobs: Vec<String>,
}

impl ShellState {
    /// Read from `BRO_*` variables set by the widget. Fields are separated by the ASCII record
    /// separator (0x1e), since commands can contain newlines and spaces.
    pub fn from_env() -> ShellState {
        let list = |name: &str| -> Vec<String> {
            std::env::var(name)
                .map(|v| v.split(['\x1e', ' ']).filter(|s| !s.is_empty()).map(String::from).collect())
                .unwrap_or_default()
        };
        ShellState {
            history: std::env::var("BRO_HISTORY")
                .ok()
                .map(|v| v.split('\x1e').filter(|s| !s.trim().is_empty()).map(String::from).collect()),
            last_status: std::env::var("BRO_STATUS").ok().and_then(|s| s.parse().ok()),
            aliases: list("BRO_ALIASES"),
            functions: list("BRO_FUNCTIONS"),
            jobs: std::env::var("BRO_JOBS")
                .map(|v| v.split('\x1e').filter(|s| !s.is_empty()).map(String::from).collect())
                .unwrap_or_default(),
        }
    }
}

/// How long one source may take before it's skipped.
pub const SOURCE_TIME_LIMIT: Duration = Duration::from_millis(30);

enum Part {
    Recent(Vec<RecentCommand>),
    Folder(String, Vec<String>, usize),
    Git(Git),
    GitStatus(String),
    Ctx(ctx::CtxContext),
    Processes(Vec<Process>),
    Index(index::Index),
    Examples(Vec<learned::Example>),
    Manuals(Vec<man::Manual>),
    Tldr(Vec<tldr::Example>),
    System(system::System),
}

/// Gather live context for `request`. Sources run in parallel; results that arrive after the
/// deadline are dropped.
pub fn gather(request: &str, line: &str, shell: ShellState, config: &Config, timings: &mut Timings) -> Context {
    let started = Instant::now();
    // Every source reports, with `None` when it has nothing, so the wait ends as soon as all
    // sources are done rather than at the deadline.
    let (tx, rx) = mpsc::channel::<(&'static str, Option<Part>, Duration)>();
    let words = request_words(&format!("{request} {line}"));
    let in_ctx = std::env::var_os("CTX_TAB").is_some();

    let spawn = |name: &'static str,
                 tx: &mpsc::Sender<(&'static str, Option<Part>, Duration)>,
                 f: Box<dyn FnOnce() -> Option<Part> + Send>| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            let part = f();
            let _ = tx.send((name, part, t.elapsed()));
        });
    };

    let mut expected = 0;
    {
        let history = shell.history.clone();
        let last_status = shell.last_status;
        expected += 1;
        let count = config.recent_commands;
        let this_request = request.trim().to_string();
        spawn(
            "history",
            &tx,
            Box::new(move || {
                let mut recent = history::recent(history, last_status, count + 1);
                // The widget enters this request as `# bro …` before asking; it isn't history yet.
                let same = |a: &str| norm(a) == norm(&this_request);
                if recent.last().and_then(|r| learned::asked(&r.command)).is_some_and(same) {
                    recent.pop();
                }
                // The exit status is the last real command's: `# bro …` comments don't change it.
                let status = recent.iter_mut().filter_map(|r| r.exit_status.take()).last();
                if let Some(real) = recent.iter_mut().rev().find(|r| learned::asked(&r.command).is_none()) {
                    real.exit_status = status.or(last_status);
                }
                let start = recent.len().saturating_sub(count);
                recent.drain(..start);
                learned::annotate(&mut recent);
                Some(Part::Recent(recent))
            }),
        );
    }
    expected += 1;
    spawn(
        "folder",
        &tx,
        Box::new(|| {
            let (folder, files, total) = folder::read();
            Some(Part::Folder(folder, files, total))
        }),
    );
    expected += 1;
    spawn("git", &tx, Box::new(|| folder::git().map(Part::Git)));
    let stop_git = Arc::new(AtomicBool::new(false));
    {
        let stop = stop_git.clone();
        expected += 1;
        spawn("git status", &tx, Box::new(move || folder::git_status(stop).map(Part::GitStatus)));
    }
    if in_ctx {
        let words = words.clone();
        expected += 1;
        spawn("ctx", &tx, Box::new(move || ctx::read(&words).map(Part::Ctx)));
    }
    {
        let words = words.clone();
        expected += 1;
        spawn("processes", &tx, Box::new(move || Some(Part::Processes(processes::sample(&words)))));
    }
    {
        let words = words.clone();
        expected += 1;
        spawn("index", &tx, Box::new(move || index::load_relevant(&words).map(Part::Index)));
    }
    {
        let words = words.clone();
        let limit = config.history_examples;
        expected += 1;
        spawn("examples", &tx, Box::new(move || Some(Part::Examples(learned::relevant(&words, limit)))));
    }
    {
        let request = request.to_string();
        let line = line.to_string();
        expected += 1;
        spawn("manuals", &tx, Box::new(move || Some(Part::Manuals(man::for_request(&request, &line)))));
    }
    {
        let request = request.to_string();
        let line = line.to_string();
        let limit = tldr_limit(config);
        expected += 1;
        spawn("tldr", &tx, Box::new(move || Some(Part::Tldr(tldr::relevant(&request, &line, limit)))));
    }
    expected += 1;
    spawn("system", &tx, Box::new(|| Some(Part::System(system::read()))));
    drop(tx);
    timings.mark("sources started");

    let mut context = Context {
        request: request.to_string(),
        line: line.to_string(),
        environment: environment(),
        aliases: shell.aliases,
        functions: shell.functions,
        jobs: shell.jobs,
        ..Context::default()
    };
    let deadline = started + SOURCE_TIME_LIMIT;
    // The status may arrive before the branch.
    let mut pending_status = None;
    let mut ctx_commands: Vec<RecentCommand> = Vec::new();
    let mut received = 0;
    while received < expected {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok((name, part, took)) => {
                received += 1;
                timings.record(name, took);
                let Some(part) = part else { continue };
                match part {
                    Part::Recent(r) => context.recent_commands = r,
                    Part::Folder(folder, files, total) => {
                        context.folder = folder;
                        context.files = files;
                        context.files_total = total;
                    }
                    Part::Git(git) => context.git = Some(Git { status: pending_status.take(), ..git }),
                    Part::GitStatus(status) => match &mut context.git {
                        Some(git) => git.status = Some(status),
                        None => pending_status = Some(status),
                    },
                    Part::Ctx(c) => {
                        ctx_commands = c.recent_commands;
                        context.tabs = c.tabs;
                        context.saved_tab_commands = c.saved_tab_commands;
                    }
                    Part::Processes(p) => context.processes = p,
                    Part::Index(i) => {
                        context.apps = i.apps;
                        context.projects = i.projects;
                        context.programs = i.programs;
                        context.default_browser = i.default_browser;
                    }
                    Part::Examples(e) => context.examples = e,
                    Part::Manuals(m) => context.manuals = m,
                    Part::Tldr(t) => context.tldr = Some(t),
                    Part::System(s) => context.system = s,
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop_git.store(true, Ordering::Relaxed);
    merge_ctx_commands(&mut context.recent_commands, ctx_commands);
    timings.mark("sources done");
    context
}

/// For comparing requests: case, surrounding space and final punctuation don't matter.
fn norm(text: &str) -> String {
    text.trim().trim_end_matches(['.', '!', '?']).trim().to_lowercase()
}

/// ctx knows the output and exit status of the last few commands; the shell history knows more
/// lines. Attach ctx's details to the matching history lines, and add any the history lacks.
fn merge_ctx_commands(history: &mut Vec<RecentCommand>, from_ctx: Vec<RecentCommand>) {
    let mut search_end = history.len();
    let mut extra = Vec::new();
    for c in from_ctx.into_iter().rev() {
        match history[..search_end].iter().rposition(|h| h.command == c.command) {
            Some(i) => {
                history[i].output = c.output;
                history[i].exit_status = c.exit_status.or(history[i].exit_status);
                search_end = i;
            }
            None => extra.push(c),
        }
    }
    extra.reverse();
    history.extend(extra);
}

/// How many tldr examples a request gets.
pub fn tldr_limit(config: &Config) -> usize {
    if config.small() { 3 } else { 6 }
}

/// Environment variable names, with values only where the redaction rules allow.
fn environment() -> Vec<(String, Option<String>)> {
    let mut vars: Vec<(String, Option<String>)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let name = k.into_string().ok()?;
            if name.starts_with("BRO_") || name.starts_with('_') {
                return None;
            }
            let value = redact::env_value_allowed(&name).then(|| v.to_string_lossy().into_owned());
            Some((name, value))
        })
        .collect();
    vars.sort();
    vars
}

/// Lower-case words of the request, for relevance matching. Short filler words are dropped.
pub fn request_words(text: &str) -> Vec<String> {
    const FILLER: &[&str] = &[
        "the", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "that", "this", "my", "me", "it", "is",
        "are", "be", "all", "what", "whats", "how", "do", "i", "you", "bro", "please", "can", "from", "at", "by", "up",
        "out", "its", "but", "so", "which", "where",
    ];
    let mut words = Vec::new();
    for w in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.')) {
        let w = w.trim_matches(['.', '-', '_']).to_lowercase();
        if w.len() >= 2 && !FILLER.contains(&w.as_str()) && !words.contains(&w) {
            words.push(w);
        }
    }
    words
}

/// Crude English stemming for matching: `folders` ~ `folder`, `biggest` ~ `big`,
/// `processes` ~ `process`, `deleting` ~ `delet(e)`.
pub fn stem(word: &str) -> String {
    let w = word.to_lowercase();
    for suffix in ["sses", "ches", "shes", "xes", "iest", "est", "ing", "ed", "s"] {
        let Some(s) = w.strip_suffix(suffix) else { continue };
        if suffix == "s" && (s.ends_with('s') || s.ends_with('u') || s.ends_with('i')) {
            break; // process, status, analysis
        }
        if s.len() < 2 {
            break;
        }
        let s = match suffix {
            "sses" => format!("{s}ss"),
            "ches" | "shes" => format!("{s}{}", &suffix[..2]),
            "xes" => format!("{s}x"),
            _ => s.to_string(),
        };
        // `bigg` → `big`, `stopp` → `stop`, but not `fall` or `pass`.
        let b = s.as_bytes();
        let n = b.len();
        if n >= 2 && b[n - 1] == b[n - 2] && !matches!(b[n - 1], b'l' | b's' | b'z' | b'e' | b'o') {
            return s[..n - 1].to_string();
        }
        return s;
    }
    w
}

/// Letters and digits only, lower case: `sys-admin`, `SysAdmin` and `sysadmin` compare equal.
pub fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// True if `name` is plausibly what one of the request words refers to.
pub fn relevant(name: &str, words: &[String]) -> bool {
    let name = squash(name);
    if name.len() < 2 {
        return false;
    }
    words.iter().any(|w| {
        let w = squash(w);
        w.len() >= 3 && (name.contains(&w) || (name.len() >= 4 && w.contains(&name)))
    })
}

/// Apply the same (bounded) rules to a snapshot read from disk; used by `--context-file`.
pub fn load_snapshot(path: &str) -> Result<Context, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_drop_filler_and_punctuation() {
        assert_eq!(request_words("Bro, kill that ghostty process."), vec!["kill", "ghostty", "process"]);
        assert_eq!(request_words("what's using port 3000?"), vec!["using", "port", "3000"]);
    }

    #[test]
    fn stems() {
        for (a, b) in [
            ("folders", "folder"),
            ("biggest", "big"),
            ("processes", "process"),
            ("files", "file"),
            ("boxes", "box"),
            ("matches", "match"),
            ("running", "run"),
            ("pass", "pass"),
        ] {
            assert_eq!(stem(a), stem(b), "{a} vs {b}");
        }
    }

    #[test]
    fn relevance_ignores_separators() {
        let words = request_words("open my personal sysadmin agent");
        assert!(relevant("~/Agents/sys-admin", &words));
        assert!(relevant("Ghostty", &request_words("kill that ghostty process")));
        assert!(!relevant("Safari", &words));
    }
}
