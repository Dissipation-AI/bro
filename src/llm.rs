//! OpenAI-compatible `/chat/completions` client, streaming (SSE). The model is asked for JSON:
//! `{"command": "...", "explanation": "...", "confidence": "high|low"}`, constrained by a schema
//! where the server supports it ([`Config::wants_json_schema`]). Parsing is lenient: JSON, then a
//! fenced code block, then the first line.
//!
//! While the answer streams in, the command is decoded from the partial JSON and reported as it
//! grows, so the prompt fills before the explanation is written.

use crate::config::Config;
use crate::prompt::Message;
use serde_json::{Value, json};
use std::io::BufRead;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    pub command: String,
    pub explanation: String,
    pub confidence: String,
}

#[derive(Debug)]
pub enum Error {
    /// Couldn't connect or timed out.
    Unreachable(String),
    /// The server answered with an error status.
    Server(u16, String),
    /// The model answered, but nothing usable came out of it.
    Empty(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(e) => write!(f, "{e}"),
            Error::Server(status, body) => write!(f, "the model server answered {status}: {body}"),
            Error::Empty(raw) => write!(f, "the model gave no command (it said: {raw})"),
        }
    }
}

/// Enough for a long command and a one-line explanation; caps a model that starts rambling.
const MAX_ANSWER_TOKENS: u32 = 300;

/// A connection (with TLS) takes milliseconds; much longer means something is in the way.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// The command so far, while the answer streams in.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub command: String,
    /// The command is finished; only the explanation is still to come.
    pub complete: bool,
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {"type": "string"},
            "explanation": {"type": "string"},
            "confidence": {"type": "string", "enum": ["high", "low"]},
        },
        "required": ["command", "explanation", "confidence"],
        "additionalProperties": false,
    })
}

pub fn request_body(messages: &[Message], config: &Config, with_schema: bool) -> Value {
    let messages: Vec<Value> = messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "temperature": config.temperature,
        "max_tokens": MAX_ANSWER_TOKENS,
        "stream": true,
    });
    if with_schema {
        body["response_format"] =
            json!({"type": "json_schema", "json_schema": {"name": "answer", "strict": true, "schema": schema()}});
    }
    if !config.reasoning_effort.is_empty() {
        body["reasoning_effort"] = json!(config.reasoning_effort);
    }
    body
}

/// Ask the model; `progress` is called as the command streams in (only when it changed).
pub fn ask(messages: &[Message], config: &Config, progress: &mut dyn FnMut(&Progress)) -> Result<Answer, Error> {
    let with_schema = config.wants_json_schema();
    let content = match complete(messages, config, with_schema, progress) {
        // Servers without structured output tend to reject the field with a 400.
        Err(Error::Server(400, body))
            if with_schema && body.contains("response_format") || with_schema && body.contains("schema") =>
        {
            complete(messages, config, false, progress)?
        }
        other => other?,
    };
    if std::env::var_os("BRO_DEBUG").is_some() {
        eprintln!("bro debug: raw model answer: {content}");
    }
    let answer = parse(&content);
    if answer.command.is_empty() {
        return Err(Error::Empty(short(&content, 200)));
    }
    Ok(answer)
}

/// The answer's text, read from the SSE stream (or a plain JSON response, for servers that
/// ignore `stream`).
fn complete(
    messages: &[Message],
    config: &Config,
    with_schema: bool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<String, Error> {
    let url = format!("{}/chat/completions", config.endpoint.trim_end_matches('/'));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(config.timeout_ms)))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let body = request_body(messages, config, with_schema).to_string();
    let mut req = agent.post(&url).header("content-type", "application/json").header("accept", "text/event-stream");
    if let Some(k) = config.api_key() {
        req = req.header("authorization", &format!("Bearer {k}"));
    }
    let mut resp = req.send(body.as_bytes()).map_err(|e| Error::Unreachable(describe(&e, &config.endpoint)))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(Error::Server(status, short(&text, 300)));
    }
    let reader = std::io::BufReader::new(resp.body_mut().as_reader());
    let mut content = String::new();
    let mut whole = String::new();
    let mut last: Option<Progress> = None;
    for line in reader.lines() {
        let line = line.map_err(|e| Error::Unreachable(read_error(&e, &config.endpoint)))?;
        let Some(data) = line.strip_prefix("data:") else {
            whole.push_str(&line);
            whole.push('\n');
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(e) = v.get("error") {
            return Err(Error::Server(status, short(&e.to_string(), 300)));
        }
        if let Some(piece) = v["choices"][0]["delta"]["content"].as_str() {
            content.push_str(piece);
            if let Some(p) = partial_command(&content)
                && last.as_ref() != Some(&p)
            {
                progress(&p);
                last = Some(p);
            }
        }
    }
    if content.is_empty() && !whole.trim().is_empty() {
        // Not a stream: an ordinary completion.
        let v: Value = serde_json::from_str(&whole).map_err(|_| Error::Server(status, short(&whole, 300)))?;
        if let Some(e) = v.get("error") {
            return Err(Error::Server(status, short(&e.to_string(), 300)));
        }
        content = v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
    }
    Ok(content)
}

fn read_error(e: &std::io::Error, endpoint: &str) -> String {
    if e.kind() == std::io::ErrorKind::TimedOut || e.to_string().to_lowercase().contains("timeout") {
        format!("the model at {endpoint} took too long (timeout_ms in ~/.config/bro/config.toml)")
    } else {
        format!("lost the connection to the model at {endpoint}: {e}")
    }
}

/// The command decoded from a partial answer: the `command` string of a JSON answer as far as
/// it has arrived, or the first line of a plain answer.
pub fn partial_command(content: &str) -> Option<Progress> {
    let text = content.trim_start();
    let text = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")).unwrap_or(text).trim_start();
    if text.starts_with('{') {
        let key = text.find("\"command\"")?;
        let rest = text[key + 9..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        let (command, complete) = decode_string(rest);
        let command = clean_command(&command);
        return (!command.is_empty()).then_some(Progress { command, complete });
    }
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let (first, complete) = match text.split_once('\n') {
        Some((first, _)) => (first, true),
        None => (text, false),
    };
    let command = match split_inline_comment(first) {
        Some((cmd, _)) => cmd.to_string(),
        None => first.trim_end().to_string(),
    };
    let command = clean_command(&command);
    (!command.is_empty() && starts_like_command(&command)).then_some(Progress { command, complete })
}

/// Decode a JSON string body (after the opening quote), up to the closing quote if it has
/// arrived. Lenient: an invalid escape such as `\;` (from `find -exec … \;`) is kept as written.
fn decode_string(s: &str) -> (String, bool) {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return (out, true),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => {}
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => return (out, false),
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return (out, false),
            },
            c => out.push(c),
        }
    }
    (out, false)
}

fn describe(e: &ureq::Error, endpoint: &str) -> String {
    match e {
        ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve) => blocked(endpoint),
        // LuLu closes a blocked socket under the program: EBADF.
        ureq::Error::Io(io) if io.raw_os_error() == Some(9) => blocked(endpoint),
        ureq::Error::Timeout(_) => {
            format!("the model at {endpoint} took too long (timeout_ms in ~/.config/bro/config.toml)")
        }
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            let hint = if endpoint.contains(":11434") { " Is `apfel --serve` (or Ollama) running?" } else { "" };
            format!("can't reach the model at {endpoint}: connection refused.{hint}")
        }
        other => format!("can't reach the model at {endpoint}: {other}"),
    }
}

fn blocked(endpoint: &str) -> String {
    format!(
        "can't connect to the model at {endpoint}. If a firewall such as LuLu or Little Snitch is running, \
         allow bro to connect."
    )
}

fn short(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= max {
        return one_line;
    }
    let cut = one_line.char_indices().take_while(|(i, _)| *i < max).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
    format!("{}…", &one_line[..cut])
}

/// Lenient parsing for small models: JSON (also inside prose or a fence, also with invalid
/// escapes, also cut off), then a fenced code block, then the first line (with an optional `#`
/// comment as the explanation).
pub fn parse(content: &str) -> Answer {
    if let Some(a) = parse_json(content) {
        return a;
    }
    if let Some(a) = parse_lines(content) {
        return a;
    }
    if let Some(block) = fenced_block(content) {
        return Answer { command: clean_command(&block), explanation: String::new(), confidence: "low".into() };
    }
    // A bare line is only taken if it starts like a command; prose ("I'm not sure…") is not.
    let first = clean_command(content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""));
    let command = if starts_like_command(&first) { first } else { String::new() };
    Answer { command, explanation: String::new(), confidence: "low".into() }
}

fn parse_json(content: &str) -> Option<Answer> {
    let field_of = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let (command, explanation, confidence) = match find_json(content) {
        Some(v) => (field_of(&v, "command"), field_of(&v, "explanation"), field_of(&v, "confidence")),
        None => {
            // Invalid escapes or a cut-off answer: read the fields one by one.
            let start = content.find('{')?;
            let text = &content[start..];
            let field = |k: &str| -> String {
                let Some(at) = text.find(&format!("\"{k}\"")) else { return String::new() };
                let rest = text[at + k.len() + 2..].trim_start();
                rest.strip_prefix(':')
                    .map(str::trim_start)
                    .and_then(|r| r.strip_prefix('"'))
                    .map(|r| decode_string(r).0.trim().to_string())
                    .unwrap_or_default()
            };
            (field("command"), field("explanation"), field("confidence"))
        }
    };
    let command = clean_command(&command);
    if command.is_empty() {
        return None;
    }
    let hedges = ["assum", "guess", "not sure", "unsure"].iter().any(|w| explanation.to_lowercase().contains(w));
    let confidence = if confidence.eq_ignore_ascii_case("low") || hedges { "low" } else { "high" };
    Some(Answer { command, explanation: one_line(&explanation), confidence: confidence.into() })
}

fn starts_like_command(line: &str) -> bool {
    let Some(word) = line.split_whitespace().next() else { return false };
    const BUILTINS: &[&str] =
        &["cd", "export", "source", "alias", "unset", "for", "while", "if", "eval", "exec", "sudo", "ctx"];
    word.starts_with(['/', '~', '.', '$'])
        || word.contains('=')
        || BUILTINS.contains(&word)
        || crate::context::man::on_path(word)
}

/// `command` (possibly several lines, possibly fenced) followed by `# explanation`.
fn parse_lines(content: &str) -> Option<Answer> {
    let text = content.trim();
    if text.starts_with('{') || text.contains("\"command\"") {
        return None;
    }
    let body = match fenced_block(text) {
        Some(block) if !text.starts_with("```") => return Some(split_comment(&block, text)),
        Some(block) => block,
        None => text.to_string(),
    };
    // With the `#` line the model followed the format; without it, only take a command-like line.
    Some(split_comment(&body, text))
        .filter(|a| !a.command.is_empty() && (!a.explanation.is_empty() || starts_like_command(&a.command)))
}

/// Split `command # comment` at the first ` # ` outside quotes.
fn split_inline_comment(line: &str) -> Option<(&str, &str)> {
    let (mut single, mut double) = (false, false);
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !double => single = !single,
            b'"' if !single => double = !double,
            b'#' if !single && !double && i > 0 && bytes[i - 1] == b' ' && bytes.get(i + 1) == Some(&b' ') => {
                let cmd = line[..i].trim_end();
                return (!cmd.is_empty()).then(|| (cmd, line[i + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// Command lines up to the first `#` comment line; the comment (inside or after a fence) is the
/// explanation.
fn split_comment(body: &str, whole: &str) -> Answer {
    let mut command: Vec<&str> = Vec::new();
    let mut explanation = String::new();
    for line in body.lines() {
        let t = line.trim();
        if let Some(c) = t.strip_prefix('#') {
            explanation = c.trim().to_string();
            break;
        }
        // `ctx show "Dev server" # Opens the tab.`: command and comment on one line.
        if let Some((cmd, comment)) = split_inline_comment(line) {
            command.push(cmd);
            explanation = comment.to_string();
            break;
        }
        if t.starts_with("```") || t.is_empty() {
            continue;
        }
        // A later line belongs to the command only if the command clearly continues
        // (`\`, a pipe, a loop or block); otherwise it's the model talking.
        if let Some(prev) = command.last().map(|p| p.trim_end()) {
            let continues =
                ["\\", "|", "&&", "||", ";", "do", "then", "else", "{", "("].iter().any(|e| prev.ends_with(e))
                    || line.starts_with([' ', '\t'])
                    || ["done", "fi", "}", ")", "esac", "else"].iter().any(|k| t.starts_with(k));
            if !continues {
                break;
            }
        }
        command.push(line.trim_end());
    }
    if explanation.is_empty()
        && let Some(c) = whole.lines().rev().find_map(|l| l.trim().strip_prefix("# "))
    {
        explanation = c.trim().to_string();
    }
    let command = clean_command(&command.join("\n"));
    let unsure = explanation.is_empty()
        || ["assum", "guess", "not sure", "unsure", "might", "maybe"]
            .iter()
            .any(|w| explanation.to_lowercase().contains(w));
    Answer { command, explanation: one_line(&explanation), confidence: if unsure { "low" } else { "high" }.into() }
}

fn find_json(content: &str) -> Option<Value> {
    let start = content.find('{')?;
    // Try the longest candidate first, then shorter ones ending at earlier braces.
    for (end, _) in content.rmatch_indices('}') {
        if end < start {
            break;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&content[start..=end])
            && v.is_object()
        {
            return Some(v);
        }
    }
    None
}

fn fenced_block(content: &str) -> Option<String> {
    let start = content.find("```")?;
    let after = &content[start + 3..];
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```").unwrap_or(body.len());
    let block = body[..end].trim();
    (!block.is_empty()).then(|| block.to_string())
}

/// Strip what models wrap commands in: backticks, a `$ ` prompt, trailing whitespace.
pub fn clean_command(command: &str) -> String {
    let mut c = command.trim();
    if c.starts_with("```") {
        c = c.trim_start_matches('`');
        c = c
            .split_once('\n')
            .map(|(lang, rest)| if lang.trim().chars().all(|ch| ch.is_alphanumeric()) { rest } else { c })
            .unwrap_or(c);
        c = c.trim_end_matches('`').trim();
    }
    if c.len() >= 2 && c.starts_with('`') && c.ends_with('`') {
        c = c[1..c.len() - 1].trim();
    }
    c = c.strip_prefix("$ ").unwrap_or(c);
    c.trim().to_string()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_lines() {
        let a = parse("pkill -i ghostty\n# Stops every ghostty process.");
        assert_eq!(a.command, "pkill -i ghostty");
        assert_eq!(a.explanation, "Stops every ghostty process.");
        assert_eq!(a.confidence, "high");
        let a = parse(
            "find . -name \"node_modules\" -type d -prune -exec rm -rf {} +\n# Deletes them; I assumed you meant here.",
        );
        assert_eq!(a.command, "find . -name \"node_modules\" -type d -prune -exec rm -rf {} +");
        assert_eq!(a.confidence, "low");
    }

    #[test]
    fn fenced_with_comment() {
        let a = parse("```zsh\ndu -sh * | sort -rh\n# Sizes, largest first.\n```");
        assert_eq!(a.command, "du -sh * | sort -rh");
        assert_eq!(a.explanation, "Sizes, largest first.");
        let a = parse("Here:\n```\nls -lt\n```\n# Newest first.");
        assert_eq!(a.command, "ls -lt");
        assert_eq!(a.explanation, "Newest first.");
    }

    #[test]
    fn inline_comment() {
        let a = parse("ctx show \"Dev server\" # Opens the tab with the dev server.");
        assert_eq!(a.command, "ctx show \"Dev server\"");
        assert_eq!(a.explanation, "Opens the tab with the dev server.");
        assert_eq!(parse("echo 'a # b'\n# Prints.").command, "echo 'a # b'");
        assert_eq!(parse("open \"https://x.com/#top\"\n# Opens.").command, "open \"https://x.com/#top\"");
    }

    #[test]
    fn multi_line_command() {
        let a = parse("for f in *.png; do\n  sips -Z 800 \"$f\"\ndone\n# Shrinks every PNG.");
        assert_eq!(a.command, "for f in *.png; do\n  sips -Z 800 \"$f\"\ndone");
    }

    #[test]
    fn valid_json() {
        let a = parse(r#"{"command": "pkill -i ghostty", "explanation": "Stops ghostty.", "confidence": "high"}"#);
        assert_eq!(a.command, "pkill -i ghostty");
        assert_eq!(a.explanation, "Stops ghostty.");
        assert_eq!(a.confidence, "high");
    }

    #[test]
    fn json_in_prose_and_fence() {
        let a = parse(
            "Sure! Here you go:\n```json\n{\"command\": \"ls -la\", \"explanation\": \"Lists files.\", \"confidence\": \"low\"}\n```\nHope that helps {really}.",
        );
        assert_eq!(a.command, "ls -la");
        assert_eq!(a.confidence, "low");
    }

    #[test]
    fn fenced_code() {
        let a = parse("Use this:\n```zsh\ndu -sh * | sort -rh\n```");
        assert_eq!(a.command, "du -sh * | sort -rh");
        assert_eq!(a.confidence, "low");
    }

    #[test]
    fn bare_line_and_prompt_sign() {
        assert_eq!(parse("\n  $ git status\nThis shows status.").command, "git status");
        assert_eq!(parse("`open -a Safari`").command, "open -a Safari");
    }

    #[test]
    fn prose_is_not_a_command() {
        assert_eq!(parse("I'm not sure what you mean.").command, "");
        assert_eq!(parse("").command, "");
        assert_eq!(parse("Sorry, I can't help with that").command, "");
    }

    #[test]
    fn json_with_invalid_escapes_or_cut_off() {
        // `\;` isn't a JSON escape, but it's what `find -exec` needs.
        let a = parse(r#"{"command": "find . -name \"*.log\" -exec rm {} \;", "explanation": "Deletes logs."}"#);
        assert_eq!(a.command, r#"find . -name "*.log" -exec rm {} \;"#);
        assert_eq!(a.explanation, "Deletes logs.");
        // Cut off by max_tokens during the explanation.
        let a = parse(r#"{"command": "ls -lt", "explanation": "Lists files, newe"#);
        assert_eq!(a.command, "ls -lt");
        assert_eq!(parse(r#"{"confidence": "low", "command": "ls""#).command, "ls");
    }

    #[test]
    fn command_decoded_while_streaming() {
        let p = |s: &str| partial_command(s);
        assert_eq!(p(r#"{"comm"#), None);
        assert_eq!(p(r#"{"command": "git sta"#), Some(Progress { command: "git sta".into(), complete: false }));
        assert_eq!(
            p(r#"{"command": "echo \"hi\"", "expl"#),
            Some(Progress { command: "echo \"hi\"".into(), complete: true })
        );
        assert_eq!(p("```json\n{\"command\": \"ls\""), Some(Progress { command: "ls".into(), complete: true }));
        // Plain answers: the first line is the command.
        assert_eq!(p("ls -l"), Some(Progress { command: "ls -l".into(), complete: false }));
        assert_eq!(p("ls -l\n# Lists."), Some(Progress { command: "ls -l".into(), complete: true }));
        assert_eq!(p("Sorry, I"), None);
    }

    #[test]
    fn command_inside_json_is_cleaned() {
        assert_eq!(parse(r#"{"command": "```bash\nls\n```", "explanation": "x"}"#).command, "ls");
    }
}
