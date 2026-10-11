//! Drives a real interactive zsh in a pseudo-terminal with `expect` (ships with macOS), with the
//! bro widget loaded and a fake model server. Skipped if zsh or expect is missing.

mod common;
use common::*;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn available() -> bool {
    Path::new("/bin/zsh").exists() && Path::new("/usr/bin/expect").exists()
}

/// Which terminal's shell integration to load alongside bro, the way that terminal loads it.
#[derive(Clone, Copy)]
enum Terminal {
    Plain,
    /// Ghostty points ZDOTDIR at its own `.zshenv`, which sources the user's and then its
    /// integration, which finishes setting up on the first prompt.
    Ghostty,
    /// `/etc/zshrc` sources `/etc/zshrc_Apple_Terminal` when `TERM_PROGRAM` is `Apple_Terminal`.
    Apple,
    /// iTerm2's integration is a script users source from `.zshrc` (`ITERM2_INTEGRATION` names it).
    ITerm2,
}

const GHOSTTY_ZSH: &str = "/Applications/Ghostty.app/Contents/Resources/ghostty/shell-integration/zsh";

impl Terminal {
    fn available(self) -> bool {
        match self {
            Terminal::Plain => true,
            Terminal::Ghostty => Path::new(GHOSTTY_ZSH).join("ghostty-integration").exists(),
            Terminal::Apple => Path::new("/etc/zshrc_Apple_Terminal").exists(),
            Terminal::ITerm2 => std::env::var_os("ITERM2_INTEGRATION").is_some_and(|p| Path::new(&p).exists()),
        }
    }
}

/// Set up a ZDOTDIR whose .zshrc loads the widget, then run an expect script body against it.
/// The body runs after the first prompt; `$prompt` matches the prompt.
fn run_expect(name: &str, server: &FakeServer, body: &str) -> (bool, String) {
    run_expect_in(Terminal::Plain, name, server, body)
}

fn run_expect_in(terminal: Terminal, name: &str, server: &FakeServer, body: &str) -> (bool, String) {
    let sb = Sandbox::new(name, &server.url);
    let bro = env!("CARGO_BIN_EXE_bro");
    let iterm = match terminal {
        Terminal::ITerm2 => format!("source '{}'\n", std::env::var("ITERM2_INTEGRATION").unwrap()),
        _ => String::new(),
    };
    std::fs::write(
        sb.home.join(".zshrc"),
        format!(
            "{iterm}PS1='READY> '\nHISTFILE=$HOME/.zsh_history\nSAVEHIST=100\nHISTSIZE=100\nunset zle_bracketed_paste\n\
             eval \"$('{bro}' init zsh)\"\n\
             # Ctrl-T prints the line editor's state on one line, since the screen is redrawn piecemeal.\n\
             _t_dump() {{ zle -I; print -r -- \"STATE BUFFER=[$BUFFER] NOTE=[${{POSTDISPLAY//$'\\n'/ }}]\"; }}\n\
             zle -N _t_dump; bindkey '^T' _t_dump\n"
        ),
    )
    .unwrap();
    let script = format!(
        r#"
set timeout 8
set prompt "READY> "
log_user 1
spawn env -i HOME={home} {terminal_env} LANG=en_US.UTF-8 PATH={bin_dir}:/usr/bin:/bin /bin/zsh -i
expect {{
  "READY> " {{}}
  timeout {{ puts "\nFAIL: no first prompt"; exit 1 }}
}}
{body}
puts "\nDONE"
exit 0
"#,
        home = sb.home.display(),
        terminal_env = match terminal {
            Terminal::Plain => format!("ZDOTDIR={} TERM=xterm-256color", sb.home.display()),
            Terminal::Ghostty => format!(
                "ZDOTDIR={GHOSTTY_ZSH} GHOSTTY_ZSH_ZDOTDIR={} GHOSTTY_RESOURCES_DIR={} TERM=xterm-256color \
                 TERM_PROGRAM=ghostty GHOSTTY_SHELL_FEATURES=cursor,title,sudo",
                sb.home.display(),
                Path::new(GHOSTTY_ZSH).parent().unwrap().parent().unwrap().display()
            ),
            Terminal::Apple => format!(
                "ZDOTDIR={} TERM=xterm-256color TERM_PROGRAM=Apple_Terminal TERM_SESSION_ID=bro-test",
                sb.home.display()
            ),
            Terminal::ITerm2 =>
                format!("ZDOTDIR={} TERM=xterm-256color TERM_PROGRAM=iTerm.app LC_TERMINAL=iTerm2", sb.home.display()),
        },
        bin_dir = Path::new(bro).parent().unwrap().display(),
    );
    let path = sb.home.join("test.exp");
    std::fs::write(&path, script).unwrap();
    let out = Command::new("/usr/bin/expect").arg(&path).current_dir(sb.home.join("work")).output().unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if let Ok(history) = std::fs::read_to_string(sb.home.join("Library/Application Support/bro/history.jsonl")) {
        text.push_str("\nHISTORY.JSONL\n");
        text.push_str(&history);
    }
    (out.status.success() && text.contains("DONE"), text)
}

#[test]
fn bro_line_fills_the_prompt_without_running() {
    if !available() {
        return;
    }
    let server = serve("lsof -nP -iTCP:3000 -sTCP:LISTEN\n# Shows what listens on port 3000.", Duration::ZERO);
    // Apostrophe and question mark would break an ordinary command; the widget takes the line
    // verbatim. After the fill, Ctrl-U clears the line, so nothing runs; history keeps the request.
    let body = r#"
send "bro what's using port 3000?\r"
send "\024"
expect {
  -ex {STATE BUFFER=[lsof -nP -iTCP:3000 -sTCP:LISTEN] NOTE=[   bro: Shows what listens on port 3000.]} { sleep 0.3 }
  timeout { puts "\nFAIL: prompt not filled with the explanation under it"; exit 1 }
}
send "\025"
send "\024"
expect {
  -ex {STATE BUFFER=[] NOTE=[]} { sleep 0.3 }
  timeout { puts "\nFAIL: explanation not cleared after editing"; exit 1 }
}
send "fc -ln -1\r"
expect {
  "bro what's using port 3000?" {}
  timeout { puts "\nFAIL: request not in history"; exit 1 }
}
expect $prompt
"#;
    let (ok, out) = run_expect("fill", &server, body);
    assert!(ok, "{out}");
    let sent = server.requests.lock().unwrap().join("\n");
    assert!(sent.contains("Request: what's using port 3000?"), "{sent}");
}

#[test]
fn voice_variants_and_non_matches() {
    if !available() {
        return;
    }
    let server = serve("open -a Safari\n# Opens Safari.", Duration::ZERO);
    let body = r#"
send "Bro, open safari! now.\r"
send "\024"
expect {
  -ex {STATE BUFFER=[open -a Safari]} { sleep 0.3 }
  timeout { puts "\nFAIL: Bro, not caught"; exit 1 }
}
send "\025"
send "\024"
expect {
  -ex {STATE BUFFER=[] NOTE=[]} { sleep 0.3 }
  timeout { puts "\nFAIL: Ctrl-U did not clear"; exit 1 }
}
send "brown\r"
expect {
  "command not found: brown" {}
  timeout { puts "\nFAIL: brown was caught"; exit 1 }
}
expect $prompt
send "bro --version\r"
expect {
  "bro 0.1.0" {}
  timeout { puts "\nFAIL: flags not passed through"; exit 1 }
}
expect $prompt
"#;
    let (ok, out) = run_expect("voice", &server, body);
    assert!(ok, "{out}");
    let sent = server.requests.lock().unwrap();
    assert_eq!(sent.len(), 1, "only the Bro, line reached the model");
    // `!` would be history expansion in a normal command; the widget passes it through untouched.
    assert!(sent[0].contains("Request: open safari! now"), "{}", sent[0]);
}

#[test]
fn ctrl_g_finishes_the_typed_line() {
    if !available() {
        return;
    }
    let server = serve("tar --exclude=node_modules -czf site.tgz .\n# Archives the folder.", Duration::ZERO);
    let body = r#"
send "tar -czf site.tgz but skip node"
send "\007"
send "\024"
expect {
  -ex {STATE BUFFER=[tar --exclude=node_modules -czf site.tgz .]} { sleep 0.3 }
  timeout { puts "\nFAIL: Ctrl-G did not fill"; exit 1 }
}
send "\025"
"#;
    let (ok, out) = run_expect("ctrlg", &server, body);
    assert!(ok, "{out}");
    let sent = server.requests.lock().unwrap().join("\n");
    assert!(sent.contains("Typed so far: tar -czf site.tgz but skip node"), "{sent}");
}

#[test]
fn ctrl_c_cancels_a_slow_answer() {
    if !available() {
        return;
    }
    let server = serve("echo too late\n# Late.", Duration::from_secs(4));
    // Ctrl-C stops bro and leaves the request in the prompt to reword; the late answer never lands.
    let body = r#"
send "bro something slow\r"
expect {
  "cooking" {}
  timeout { puts "\nFAIL: no cooking message"; exit 1 }
}
sleep 0.5
send "\003"
sleep 0.3
send "\024"
expect {
  -ex {STATE BUFFER=[bro something slow] NOTE=[   bro: cancelled]} { sleep 0.3 }
  timeout { puts "\nFAIL: request not put back after Ctrl-C"; exit 1 }
}
send "\025"
send "echo still-alive\r"
expect {
  "still-alive" {}
  timeout { puts "\nFAIL: shell unusable after Ctrl-C"; exit 1 }
}
expect $prompt
sleep 4
send "\024"
expect {
  -ex {STATE BUFFER=[]} { sleep 0.3 }
  timeout { puts "\nFAIL: late answer landed"; exit 1 }
}
"#;
    let (ok, out) = run_expect("cancel", &server, body);
    assert!(ok, "{out}");
    assert!(!out.contains("echo too late\r\n"), "late answer must not fill the prompt: {out}");
}

#[test]
fn danger_is_marked() {
    if !available() {
        return;
    }
    let server = serve("rm -rf node_modules\n# Deletes node_modules.", Duration::ZERO);
    let body = r#"
send "bro delete node_modules\r"
send "\024"
expect {
  -ex {deletes folders and everything in them. Deletes node_modules.]} { sleep 0.3 }
  timeout { puts "\nFAIL: no danger mark"; exit 1 }
}
send "\025"
"#;
    let (ok, out) = run_expect("danger", &server, body);
    assert!(ok, "{out}");
}

#[test]
fn records_what_was_actually_run() {
    if !available() {
        return;
    }
    let server = serve("ls -l\n# Lists files.", Duration::ZERO);
    // Suggestions are edited before running (`ls -l` → `ls -l notes.txt`, then a failing
    // `ls -l /nonexistent`); both outcomes are recorded with what ran and its exit status.
    let body = r#"
send "bro list my files\r"
send "\024"
expect {
  -ex {STATE BUFFER=[ls -l]} { sleep 0.3 }
  timeout { puts "\nFAIL: not filled"; exit 1 }
}
send " notes.txt\r"
expect {
  "notes.txt" {}
  timeout { puts "\nFAIL: edited command did not run"; exit 1 }
}
expect $prompt
send "bro list again\r"
send "\024"
expect {
  -ex {STATE BUFFER=[ls -l]} { sleep 0.3 }
  timeout { puts "\nFAIL: not filled again"; exit 1 }
}
send " /nonexistent\r"
expect {
  "No such file" {}
  timeout { puts "\nFAIL: second command did not run"; exit 1 }
}
expect $prompt
sleep 1
"#;
    let (ok, out) = run_expect("learn", &server, body);
    assert!(ok, "{out}");
    let history = out.split("\nHISTORY.JSONL\n").nth(1).expect("history.jsonl written");
    let records: Vec<serde_json::Value> = history.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(records.len(), 2, "{out}");
    assert_eq!(records[0]["request"], "list my files");
    assert_eq!(records[0]["suggested"], "ls -l");
    assert_eq!(records[0]["ran"], "ls -l notes.txt");
    assert_eq!(records[0]["exit_status"], 0);
    assert_eq!(records[1]["request"], "list again");
    assert_eq!(records[1]["ran"], "ls -l /nonexistent");
    assert_eq!(records[1]["exit_status"], 1);
}

/// The fill, the explanation, clearing it on edit, and learning what ran, with each terminal's
/// own shell integration loaded too (they also hook prompts, line editing and command start).
fn fill_and_learn_in(terminal: Terminal, name: &str) {
    if !available() || !terminal.available() {
        eprintln!("skipped: {name} integration not installed");
        return;
    }
    let server = serve("ls -l\n# Lists files.", Duration::ZERO);
    // Make sure the terminal's own hooks really are installed (Ghostty's after the first prompt).
    let hook = match terminal {
        Terminal::Plain => "_bro_precmd",
        Terminal::Ghostty => "_ghostty_precmd",
        Terminal::Apple => "update_terminal_cwd",
        Terminal::ITerm2 => "iterm2_precmd",
    };
    let body = format!(
        r#"
send "\r"
expect $prompt
send "print -r -- HOOKS=\$precmd_functions=\r"
expect {{
  -re {{HOOKS=[^\r]*{hook}[^\r]*=}} {{}}
  timeout {{ puts "\nFAIL: {hook} not installed"; exit 1 }}
}}
expect $prompt
"#
    ) + r#"
send "bro what's in here?\r"
send "\024"
expect {
  -ex {STATE BUFFER=[ls -l] NOTE=[   bro: Lists files.]} { sleep 0.3 }
  timeout { puts "\nFAIL: not filled"; exit 1 }
}
send "\025"
send "\024"
expect {
  -ex {STATE BUFFER=[] NOTE=[]} { sleep 0.3 }
  timeout { puts "\nFAIL: explanation not cleared"; exit 1 }
}
send "bro list my files\r"
send "\024"
expect {
  -ex {STATE BUFFER=[ls -l]} { sleep 0.3 }
  timeout { puts "\nFAIL: not filled the second time"; exit 1 }
}
send " notes.txt\r"
expect {
  "notes.txt" {}
  timeout { puts "\nFAIL: edited command did not run"; exit 1 }
}
expect $prompt
send "ls"
send "\007"
send "\024"
expect {
  -ex {STATE BUFFER=[ls -l]} { sleep 0.3 }
  timeout { puts "\nFAIL: Ctrl-G did nothing"; exit 1 }
}
send "\025"
sleep 1
"#;
    let (ok, out) = run_expect_in(terminal, name, &server, &body);
    assert!(ok, "{out}");
    let history = out.split("\nHISTORY.JSONL\n").nth(1).expect("history.jsonl written");
    let last: serde_json::Value = serde_json::from_str(history.lines().last().unwrap()).unwrap();
    assert_eq!(last["request"], "list my files", "{out}");
    assert_eq!(last["ran"], "ls -l notes.txt", "{out}");
    assert_eq!(last["exit_status"], 0, "{out}");
}

#[test]
fn works_in_ghostty() {
    fill_and_learn_in(Terminal::Ghostty, "ghostty");
}

#[test]
fn works_in_apple_terminal() {
    fill_and_learn_in(Terminal::Apple, "apple-terminal");
}

#[test]
fn works_with_iterm2_integration() {
    fill_and_learn_in(Terminal::ITerm2, "iterm2");
}

#[test]
fn prompt_fills_as_the_answer_streams() {
    if !available() {
        return;
    }
    // Four characters every 150 ms: the command shows up while the explanation is still coming.
    let answer = r#"{"command": "du -sh * | sort -rh", "explanation": "Sizes of everything here, largest first.", "confidence": "high"}"#;
    let server = serve_with(answer, Duration::ZERO, Some(Duration::from_millis(150)));
    let body = r#"
send "bro how big is everything here\r"
# zle redraws only what changed, so match pieces: the command arrives before the explanation.
expect {
  -ex {sort -rh} {}
  timeout { puts "\nFAIL: command never streamed in"; exit 1 }
}
expect {
  -ex {largest first.} {}
  timeout { puts "\nFAIL: explanation never came"; exit 1 }
}
send "\024"
expect {
  -ex {STATE BUFFER=[du -sh * | sort -rh] NOTE=[   bro: Sizes of everything here, largest first.]} { sleep 0.3 }
  timeout { puts "\nFAIL: final state wrong"; exit 1 }
}
send "\025"
"#;
    let (ok, out) = run_expect("stream", &server, body);
    assert!(ok, "{out}");
}

#[test]
fn ctrl_c_mid_stream_puts_the_line_back() {
    if !available() {
        return;
    }
    let answer = r#"{"command": "tar --exclude=node_modules -czf site.tgz .", "explanation": "Archives it.", "confidence": "high"}"#;
    let server = serve_with(answer, Duration::ZERO, Some(Duration::from_millis(200)));
    let body = r#"
send "tar -czf site.tgz but skip node"
send "\007"
expect {
  -ex {exclude} {}
  timeout { puts "\nFAIL: nothing streamed"; exit 1 }
}
send "\003"
sleep 0.5
send "\024"
expect {
  -ex {STATE BUFFER=[tar -czf site.tgz but skip node]} { sleep 0.3 }
  timeout { puts "\nFAIL: typed line not put back after Ctrl-C"; exit 1 }
}
send "\025"
send "echo still-alive\r"
expect {
  "still-alive" {}
  timeout { puts "\nFAIL: shell unusable after Ctrl-C"; exit 1 }
}
expect $prompt
"#;
    let (ok, out) = run_expect("stream-cancel", &server, body);
    assert!(ok, "{out}");
    assert!(!out.contains("BUFFER=[tar --exclude"), "a cancelled half-answer must not stay in the prompt: {out}");
}
