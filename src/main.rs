//! bro: turns what you meant into a shell command, using the context of your terminal session.
//! It never runs anything; it fills your prompt (through the zsh widget) or prints the command.

mod config;
mod context;
mod llm;
mod prompt;
mod safety;
mod setup;
mod timing;
mod zsh;

use config::Config;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

const HELP: &str = "bro turns what you meant into a shell command. It never runs anything.

In zsh (after `bro install`), type `bro` and what you want, then Enter:
  bro kill that ghostty process
Your next prompt is filled with the command. Press Enter to run it, or edit it first.
Ctrl-G sends a half-typed command line to bro to finish it.

As a plain command, the command goes to stdout and the explanation to stderr:
  bro list the five biggest files here
  echo \"list the five biggest files here\" | bro --json

Pipe something into bro to show it what to work with. bro passes it through, like tee, and
keeps it for later requests in this terminal:
  cat ~/.config/bro/config.toml | bro set the web search to google

Setup:
  bro install          add bro to ~/.zshrc
  bro config           choose a model endpoint (apfel, Ollama, llama.cpp, LM Studio, Gemini…)
  bro config --check   send a tiny request and report how long it took
  bro init zsh         print the zsh integration (used by ~/.zshrc)

Options:
  --json               answer as JSON: command, explanation, confidence, dangerous
  --show-prompt        print exactly what would be sent to the model, and don't send it
                       (with --json: the request body itself)
  --endpoint <url>     use this OpenAI-compatible endpoint instead of the configured one
  --model <name>       use this model instead of the configured one
  --context-file <f>   use a saved context snapshot instead of reading this machine (evals)
  --line <text>        a half-typed command line to finish
  --timings            print how long each step took, to stderr
  --version            print the version";

#[derive(Default)]
struct Args {
    json: bool,
    show_prompt: bool,
    timings: bool,
    zle: bool,
    check: bool,
    record: bool,
    refresh_cache: bool,
    help: bool,
    version: bool,
    endpoint: Option<String>,
    model: Option<String>,
    context_file: Option<String>,
    line: Option<String>,
    words: Vec<String>,
}

/// Flags come first; the first word that isn't a flag starts the request, and everything from
/// there on is the request verbatim (`bro find files named --foo` keeps `--foo`).
fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    use lexopt::prelude::*;
    let mut a = Args::default();
    let mut parser = lexopt::Parser::from_iter(std::iter::once(String::new()).chain(raw));
    let text = |v: std::ffi::OsString| v.into_string().map_err(|_| "arguments must be UTF-8".to_string());
    while let Some(arg) = parser.next().map_err(|e| e.to_string())? {
        match arg {
            Long("json") => a.json = true,
            Long("show-prompt") => a.show_prompt = true,
            Long("timings") => a.timings = true,
            Long("zle") => a.zle = true,
            Long("check") => a.check = true,
            Long("record") => a.record = true,
            Long("refresh-cache") => a.refresh_cache = true,
            Long("help") | Short('h') => a.help = true,
            Long("version") | Short('V') => a.version = true,
            Long("endpoint") => a.endpoint = Some(text(parser.value().map_err(|e| e.to_string())?)?),
            Long("model") => a.model = Some(text(parser.value().map_err(|e| e.to_string())?)?),
            Long("context-file") => a.context_file = Some(text(parser.value().map_err(|e| e.to_string())?)?),
            Long("line") => a.line = Some(text(parser.value().map_err(|e| e.to_string())?)?),
            Value(first) => {
                a.words.push(text(first)?);
                for rest in parser.raw_args().map_err(|e| e.to_string())? {
                    a.words.push(text(rest)?);
                }
            }
            other => return Err(format!("{} (bro --help lists the options)", other.unexpected())),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("bro: {e}");
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("bro {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.refresh_cache {
        return exit(context::index::refresh());
    }
    if args.record {
        return exit(record_from_env());
    }

    // Subcommands only when the words are exactly the subcommand, so `bro config my git email`
    // is still a request.
    let words: Vec<&str> = args.words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["install"] => return exit(zsh::install()),
        ["init", "zsh"] => {
            let config = Config::load().unwrap_or_default();
            print!("{}", zsh::init_script(&config));
            return ExitCode::SUCCESS;
        }
        ["config"] if args.check => return exit(setup::check(&load_config(&args).unwrap_or_default())),
        ["config"] => return exit(setup::interactive()),
        _ => {}
    }

    let mut timings = timing::Timings::new(args.timings);
    let config = match load_config(&args) {
        Ok(c) => c,
        Err(e) => return fail(&args, &format!("bad config: {e}")),
    };

    let mut request = args.words.join(" ");
    let stdin_is_pipe = !std::io::stdin().is_terminal();
    // `… | bro <request>`: the piped text is something to look at, shown as it passes through
    // (like tee) unless stdout is for --json or --show-prompt.
    let tee = stdin_is_pipe && !request.trim().is_empty() && !args.zle && !args.json && !args.show_prompt;
    let mut piped: Option<context::Piped> = None;
    if stdin_is_pipe && request.trim().is_empty() {
        let mut stdin = String::new();
        let _ = std::io::stdin().read_to_string(&mut stdin);
        request = stdin;
    } else if stdin_is_pipe && !args.zle {
        let output = if tee {
            context::piped::tee(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
        } else {
            context::piped::tee(&mut std::io::stdin().lock(), &mut std::io::sink())
        };
        if !output.trim().is_empty() {
            let command = std::env::var("BRO_COMMAND_LINE").ok().and_then(|line| context::piped::source_command(&line));
            piped = Some(context::Piped { command, output, time: 0 });
        }
    }
    let session = context::piped::session();
    // Something actually came through (stdin may be /dev/null in scripts).
    let tee = tee && piped.is_some();
    let request = clean_request(&request);
    let line = args.line.clone().unwrap_or_default();
    let request = if request.is_empty() && !line.trim().is_empty() {
        "finish or fix this command line".to_string()
    } else {
        request
    };
    if request.is_empty() && args.context_file.is_none() {
        eprintln!("{HELP}");
        return ExitCode::from(2);
    }
    timings.mark("arguments read");

    let context = match &args.context_file {
        Some(path) => match context::load_snapshot(path) {
            Ok(mut c) => {
                if !request.is_empty() {
                    c.request = request.clone();
                }
                if !line.is_empty() {
                    c.line = line.clone();
                }
                if c.request.trim().is_empty() && !c.line.trim().is_empty() {
                    c.request = "finish or fix this command line".into();
                }
                c.request = clean_request(&c.request);
                if piped.is_some() {
                    c.piped = piped.clone();
                }
                // tldr knowledge is about this machine's tools, so evals look it up here too.
                if c.tldr.is_none() {
                    c.tldr = Some(context::tldr::relevant(&c.request, &c.line, context::tldr_limit(&config)));
                }
                c
            }
            Err(e) => return fail(&args, &e),
        },
        None => {
            let mut c = context::gather(&request, &line, context::ShellState::from_env(), &config, &mut timings);
            c.earlier_piped = context::piped::earlier(&session);
            c.notes = read_notes();
            c.piped = piped.clone();
            c
        }
    };
    let messages = prompt::build(&context, &config);
    timings.mark("prompt built");

    if args.show_prompt && args.json {
        println!("{}", llm::request_body(&messages, &config, config.wants_json_schema()));
        return ExitCode::SUCCESS;
    }
    if args.show_prompt {
        for m in &messages {
            println!("--- {} ---\n{}\n", m.role, m.content);
        }
        let tokens: usize = messages.iter().map(|m| prompt::estimate_tokens(&m.content)).sum();
        println!("--- about {tokens} tokens; nothing was sent ---");
        timings.print();
        return ExitCode::SUCCESS;
    }

    if let Some(p) = &piped
        && args.context_file.is_none()
    {
        context::piped::save(&session, p);
    }
    let cooking = tee && std::io::stderr().is_terminal();
    if cooking {
        eprint!("\x1b[2mbro is cooking…\x1b[0m");
    }

    if args.zle {
        // So the widget can stop bro when you press Ctrl-C.
        println!("pid\t{}", std::process::id());
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    // For the widget, the command as it streams in, then once complete with its danger flag.
    let mut sent_command: Option<String> = None;
    let mut first_token = true;
    let mut progress = |p: &llm::Progress| {
        if first_token {
            timings.mark("first command text");
            first_token = false;
        }
        if !args.zle {
            return;
        }
        if p.complete {
            if sent_command.as_deref() != Some(&p.command) {
                zle_command(&p.command, safety::danger(&p.command, &cwd).is_some());
                sent_command = Some(p.command.clone());
            }
        } else {
            println!("partial\t{}", zle_field(&p.command));
        }
    };
    let result = llm::ask(&messages, &config, &mut progress);
    timings.mark("model answered");
    if cooking {
        eprint!("\r\x1b[K");
    }
    let answer = match result {
        Ok(a) => a,
        Err(e) => {
            timings.print();
            return fail(&args, &e.to_string());
        }
    };
    let danger = safety::danger(&answer.command, &cwd);
    timings.mark("danger checked");
    if args.zle && sent_command.as_deref() != Some(&answer.command) {
        zle_command(&answer.command, danger.is_some());
    }
    if tee {
        output_after_tee(&session, &request, &answer, danger.as_deref());
    } else {
        output(&args, &answer, danger.as_deref());
    }
    timings.print();
    ExitCode::SUCCESS
}

/// After `… | bro <request>`: stdout carried the piped text, so the command goes to the next
/// prompt through the zsh integration (`BRO_SESSION` is set when it's loaded). Without it, the
/// command follows the text on a terminal, or goes to stderr so a pipe stays clean.
fn output_after_tee(session: &str, request: &str, answer: &llm::Answer, danger: Option<&str>) {
    let explanation = explain(answer, danger);
    let integrated = std::env::var_os("BRO_SESSION").is_some();
    if integrated && context::piped::write_pending(session, &answer.command, danger.is_some(), &explanation, request) {
        return;
    }
    if std::io::stdout().is_terminal() {
        println!("{}", answer.command);
    } else {
        eprintln!("{}", answer.command);
    }
    print_explanation(&explanation, danger.is_some());
}

fn explain(answer: &llm::Answer, danger: Option<&str>) -> String {
    match danger {
        Some(reason) if answer.explanation.is_empty() => format!("⚠ {reason}"),
        Some(reason) => format!("⚠ {reason}. {}", answer.explanation),
        None => answer.explanation.clone(),
    }
}

fn print_explanation(explanation: &str, dangerous: bool) {
    if explanation.is_empty() {
        return;
    }
    let stderr = std::io::stderr();
    let colour = stderr.is_terminal();
    let mut e = stderr.lock();
    let _ = match (dangerous, colour) {
        (true, true) => writeln!(e, "\x1b[31m{explanation}\x1b[0m"),
        (false, true) => writeln!(e, "\x1b[2m{explanation}\x1b[0m"),
        _ => writeln!(e, "{explanation}"),
    };
}

/// `~/.config/bro/bro.md`, if there is one (its start, if it's huge).
fn read_notes() -> String {
    let Ok(file) = std::fs::File::open(config::notes_path()) else { return String::new() };
    let mut text = String::new();
    let _ = file.take(64 * 1024).read_to_string(&mut text);
    text
}

fn load_config(args: &Args) -> Result<Config, String> {
    let mut config = Config::load()?;
    if let Some(e) = &args.endpoint {
        config.endpoint = e.clone();
    }
    if let Some(m) = &args.model {
        config.model = m.clone();
    }
    Ok(config)
}

/// Dictation adds a capital, a comma or a full stop; the widget passes the line after `bro`.
/// Trim those, and a leading `bro` when called as a plain command with the whole sentence.
fn clean_request(text: &str) -> String {
    let mut t = text.trim();
    let lower = t.to_lowercase();
    if lower.starts_with("bro") && lower[3..].starts_with(|c: char| c.is_ascii_punctuation() || c.is_whitespace()) {
        t = t[3..].trim_start_matches(|c: char| c.is_ascii_punctuation()).trim_start();
    }
    t.trim_end_matches(['.', '!']).trim().to_string()
}

fn output(args: &Args, answer: &llm::Answer, danger: Option<&str>) {
    let explanation = explain(answer, danger);
    if args.zle {
        println!("explanation\t{}", zle_field(&explanation));
        return;
    }
    if args.json {
        let v = serde_json::json!({
            "command": answer.command,
            "explanation": answer.explanation,
            "confidence": answer.confidence,
            "dangerous": danger.is_some(),
            "danger_reason": danger,
        });
        println!("{v}");
        return;
    }
    println!("{}", answer.command);
    print_explanation(&explanation, danger.is_some());
}

/// The widget reads one record per line: `pid`, `partial`, `command`, `explanation` or `error`, a tab,
/// then the fields. Newlines inside a field are sent as 0x1f.
fn zle_field(text: &str) -> String {
    text.replace('\n', "\x1f").replace('\t', " ")
}

fn zle_command(command: &str, dangerous: bool) {
    println!("command\t{}\t{}", if dangerous { "danger" } else { "ok" }, zle_field(command));
}

fn fail(args: &Args, message: &str) -> ExitCode {
    if args.zle {
        println!("error\tbro: {}", zle_field(message));
    } else if args.json {
        println!("{}", serde_json::json!({"error": message}));
    } else {
        eprintln!("bro: {message}");
    }
    ExitCode::FAILURE
}

fn exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bro: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Called by the zsh integration after the command following a suggestion (or a new request).
fn record_from_env() -> Result<(), String> {
    let var = |n: &str| std::env::var(n).ok();
    let r = context::learned::Record {
        time: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        folder: std::env::current_dir().map(|d| context::folder::tilde(&d)).unwrap_or_default(),
        request: var("BRO_REC_REQUEST").unwrap_or_default(),
        suggested: var("BRO_REC_SUGGESTED").unwrap_or_default(),
        ran: var("BRO_REC_RAN"),
        exit_status: var("BRO_REC_STATUS").and_then(|s| s.parse().ok()),
    };
    if r.request.trim().is_empty() {
        return Ok(());
    }
    context::learned::record(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Args {
        parse_args(s.iter().map(|x| x.to_string()).collect()).unwrap()
    }

    #[test]
    fn flags_then_request_verbatim() {
        let a = args(&["--json", "find", "files", "named", "--foo"]);
        assert!(a.json);
        assert_eq!(a.words, ["find", "files", "named", "--foo"]);
        let a = args(&["--line=tar -x", "--", "--weird", "request"]);
        assert_eq!(a.line.as_deref(), Some("tar -x"));
        assert_eq!(a.words, ["--weird", "request"]);
        assert_eq!(args(&["config", "--check"]).words, ["config", "--check"]);
        assert!(parse_args(vec!["--nope".into()]).is_err());
        assert!(parse_args(vec!["--model".into()]).is_err());
    }

    #[test]
    fn dictation_cleanup() {
        assert_eq!(
            clean_request("Bro, open a browser searching for salsa dancing videos."),
            "open a browser searching for salsa dancing videos"
        );
        assert_eq!(clean_request("  what's using port 3000?  "), "what's using port 3000?");
        assert_eq!(clean_request("brown paper"), "brown paper");
    }
}
