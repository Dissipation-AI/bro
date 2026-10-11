//! Man page excerpts for commands named in the request or the half-typed line, so the model uses
//! the flags *this* machine's version supports (BSD `sed`, `find`, `date` differ from GNU).
//!
//! Pages are located without spawning `man -w` (it takes ~80 ms), rendered once with macOS's
//! `mandoc`, cached by path and modification time, then trimmed to NAME, SYNOPSIS and the option
//! entries that best match the request. A `tldr` page is added if one is installed. Commands are
//! never run with `--help`.

use crate::config::cache_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Manual {
    pub command: String,
    pub text: String,
}

const MAX_COMMANDS: usize = 2;
/// Characters per excerpt, about 350 tokens.
const EXCERPT_CHARS: usize = 1_400;

/// English words that are also commands. In a natural-language request they're only taken as
/// commands when they lead the request ("find files over 1 GB").
const ENGLISH: &[&str] = &[
    "find", "kill", "open", "sort", "head", "tail", "cut", "join", "split", "time", "test", "true", "false", "yes",
    "which", "who", "say", "look", "last", "more", "less", "file", "top", "date", "make", "touch", "wait", "watch",
    "read", "set", "type", "echo", "list", "sleep", "clear", "what", "where", "when", "history", "copy", "paste",
    "show", "start", "stop", "run", "go", "install", "update", "units", "leave", "write", "talk", "comm", "fold",
    "paste", "uniq", "tr", "w", "id", "groups", "login", "lock", "script", "dig", "host", "ping", "mail", "users",
    "size", "strings", "nice", "screen", "column", "expand", "from", "at", "cal", "env", "info", "man", "help", "open",
    "can", "do", "it", "my", "this", "that", "all", "files", "folder", "use",
];

/// Shell builtins and keywords: no useful man page of their own.
const BUILTINS: &[&str] = &[
    "cd",
    "export",
    "alias",
    "unset",
    "source",
    "eval",
    "exec",
    "exit",
    "return",
    "local",
    "if",
    "then",
    "else",
    "fi",
    "for",
    "while",
    "do",
    "done",
    "case",
    "esac",
    "function",
    "sudo",
    "time",
    "builtin",
    "command",
    "noglob",
    "nocorrect",
    "bro",
    "ctx",
];

pub fn for_request(request: &str, line: &str) -> Vec<Manual> {
    let words = super::request_words(&format!("{request} {line}"));
    commands_named(request, line)
        .into_iter()
        .filter_map(|cmd| {
            let page = find_page(&cmd)?;
            let rendered = rendered(&page)?;
            let mut text = excerpt(&rendered, &words, line);
            if let Some(tldr) = tldr_page(&cmd) {
                text.push_str("\nExamples (tldr):\n");
                text.push_str(&tldr);
            }
            Some(Manual { command: cmd, text })
        })
        .collect()
}

/// Commands the request or line refers to, most certain first.
fn commands_named(request: &str, line: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let add = |w: &str, found: &mut Vec<String>| {
        let w = w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'));
        if w.is_empty() || w.starts_with('-') || BUILTINS.contains(&w) || found.iter().any(|f| f == w) {
            return;
        }
        if on_path(w) {
            found.push(w.to_string());
        }
    };
    // In a command line, the first word of each pipeline segment is a command.
    for segment in line.split(['|', ';', '&', '(', ')', '\n']) {
        let mut words = segment.split_whitespace().skip_while(|w| w.contains('=') || *w == "sudo" || *w == "env");
        if let Some(first) = words.next() {
            add(first, &mut found);
        }
    }
    // In prose, command-looking words; English ones only when they lead the request. A word
    // naming an app or followed by "process", "app" or "window" means the program, not its manual
    // ("kill that ghostty process").
    let prose: Vec<String> = request
        .split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').to_lowercase())
        .collect();
    let apps = installed_apps();
    for (i, w) in prose.iter().enumerate() {
        let leading = i == 0 || (i == 1 && prose[0] == "bro");
        let names_program = prose
            .get(i + 1)
            .is_some_and(|n| matches!(n.as_str(), "process" | "processes" | "app" | "window" | "windows"))
            || apps.iter().any(|a| a.eq_ignore_ascii_case(w));
        if w.len() >= 2 && !names_program && (leading || !ENGLISH.contains(&w.as_str())) {
            add(w, &mut found);
        }
    }
    found.truncate(MAX_COMMANDS);
    found
}

/// App names from the cached index (no walk); empty if there's no cache yet.
fn installed_apps() -> Vec<String> {
    std::fs::read(cache_dir().join("index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<super::index::Index>(&b).ok())
        .map(|i| i.apps)
        .unwrap_or_default()
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

pub fn on_path(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path_dirs()
        .iter()
        .any(|d| std::fs::metadata(d.join(command)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

fn man_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let defaults = || -> Vec<PathBuf> {
        let mut d: Vec<PathBuf> =
            vec!["/usr/share/man".into(), "/usr/local/share/man".into(), "/opt/homebrew/share/man".into()];
        for dir in path_dirs() {
            if let Some(parent) = dir.parent() {
                d.push(parent.join("share/man"));
                d.push(parent.join("man"));
            }
        }
        d.push("/Library/Developer/CommandLineTools/usr/share/man".into());
        d
    };
    match std::env::var("MANPATH") {
        Ok(mp) if !mp.is_empty() => {
            for entry in mp.split(':') {
                if entry.is_empty() {
                    roots.extend(defaults());
                } else {
                    roots.push(entry.into());
                }
            }
        }
        _ => roots = defaults(),
    }
    let mut seen = Vec::new();
    roots.retain(|r| {
        let keep = !seen.contains(r) && r.is_dir();
        seen.push(r.clone());
        keep
    });
    roots
}

fn find_page(command: &str) -> Option<PathBuf> {
    for root in man_roots() {
        for section in ["1", "8", "6"] {
            for suffix in ["", ".gz"] {
                let p = root.join(format!("man{section}")).join(format!("{command}.{section}{suffix}"));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// The rendered page, from cache or by running `mandoc` once.
fn rendered(page: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(page).ok()?;
    let key: String = page.to_string_lossy().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let cached = cache_dir().join("man").join(format!("{key}-{}.txt", meta.mtime()));
    if let Ok(text) = std::fs::read_to_string(&cached) {
        return Some(text);
    }
    // `.so` includes are relative to the man root (the parent of `man1/`).
    let root = page.parent()?.parent()?;
    let out = std::process::Command::new("/usr/bin/mandoc")
        .args(["-T", "ascii", "-O", "width=100"])
        .arg(page)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = strip_overstrike(&String::from_utf8_lossy(&out.stdout));
    if text.trim().is_empty() {
        return None;
    }
    if let Some(dir) = cached.parent() {
        let _ = std::fs::create_dir_all(dir);
        let tmp = cached.with_extension("tmp");
        if std::fs::write(&tmp, &text).is_ok() {
            let _ = std::fs::rename(tmp, &cached);
        }
    }
    Some(text)
}

/// Remove terminal bold/underline (`X\bX`, `_\bX`).
fn strip_overstrike(text: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    for c in text.chars() {
        if c == '\u{8}' {
            out.pop();
        } else {
            out.push(c);
        }
    }
    out.into_iter().collect()
}

fn stem(word: &str) -> String {
    let w = word.to_lowercase();
    for suffix in ["ing", "ed", "es", "s"] {
        if w.len() > suffix.len() + 3
            && let Some(s) = w.strip_suffix(suffix)
        {
            return s.to_string();
        }
    }
    w
}

struct Entry {
    order: usize,
    text: String,
    score: usize,
}

/// NAME, SYNOPSIS, and the option entries best matching the request, within `EXCERPT_CHARS`.
fn excerpt(rendered: &str, words: &[String], line: &str) -> String {
    let stems: Vec<String> = words.iter().map(|w| stem(w)).filter(|w| w.len() >= 3).collect();
    let typed_flags: Vec<&str> = line.split_whitespace().filter(|w| w.starts_with('-')).collect();
    let lines: Vec<&str> = rendered.lines().collect();

    let mut name = String::new();
    let mut synopsis: Vec<&str> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut section = "";
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let indent = l.len() - l.trim_start().len();
        if indent == 0 && !l.trim().is_empty() && l.trim().chars().all(|c| c.is_ascii_uppercase() || c == ' ') {
            section = l.trim();
            i += 1;
            continue;
        }
        match section {
            "NAME" if !l.trim().is_empty() => name.push_str(l.trim()),
            "SYNOPSIS" if !l.trim().is_empty() && synopsis.len() < 6 => synopsis.push(l.trim_end()),
            _ => {}
        }
        if indent > 0 && indent <= 8 && l.trim_start().starts_with('-') && section != "SYNOPSIS" {
            let mut text = l.trim().to_string();
            let mut j = i + 1;
            while j < lines.len() {
                let n = lines[j];
                let n_indent = n.len() - n.trim_start().len();
                if !n.trim().is_empty() && n_indent <= indent {
                    break;
                }
                if !n.trim().is_empty() {
                    text.push(' ');
                    text.push_str(n.trim());
                }
                j += 1;
            }
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let head = text.split_whitespace().next().unwrap_or("");
            let mut score = text.split(|c: char| !c.is_alphanumeric()).map(stem).filter(|w| stems.contains(w)).count();
            if typed_flags.iter().any(|f| head.trim_end_matches(',') == *f || head.starts_with(f)) {
                score += 10;
            }
            entries.push(Entry { order: entries.len(), text, score });
            i = j;
            continue;
        }
        i += 1;
    }

    let mut out = String::new();
    if !name.is_empty() {
        out.push_str(&name);
        out.push('\n');
    }
    if !synopsis.is_empty() {
        out.push_str(&synopsis.join("\n"));
        out.push('\n');
    }
    entries.sort_by(|a, b| b.score.cmp(&a.score).then(a.order.cmp(&b.order)));
    let mut chosen: Vec<&Entry> = Vec::new();
    let mut used = out.len();
    for e in entries.iter().filter(|e| e.score > 0) {
        let text_len = e.text.len().min(300) + 3;
        if used + text_len > EXCERPT_CHARS {
            continue;
        }
        used += text_len;
        chosen.push(e);
    }
    chosen.sort_by_key(|e| e.order);
    for e in chosen {
        out.push_str("  ");
        if e.text.len() > 300 {
            let cut =
                e.text.char_indices().take_while(|(i, _)| *i < 297).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
            out.push_str(&e.text[..cut]);
            out.push_str("...");
        } else {
            out.push_str(&e.text);
        }
        out.push('\n');
    }
    out
}

/// A tldr page from tealdeer's or the Node client's cache, reduced to its examples.
fn tldr_page(command: &str) -> Option<String> {
    let home = crate::config::home();
    let roots = [
        home.join("Library/Caches/tealdeer/tldr-pages/pages.en"),
        home.join("Library/Caches/tealdeer/tldr-pages/pages"),
        home.join(".cache/tealdeer/tldr-pages/pages.en"),
        home.join(".tldr/cache/pages"),
    ];
    for root in roots {
        for platform in ["osx", "common"] {
            if let Ok(text) = std::fs::read_to_string(root.join(platform).join(format!("{command}.md"))) {
                let mut out = String::new();
                for l in text.lines() {
                    let l = l.trim();
                    if l.starts_with("- ") || l.starts_with('`') {
                        out.push_str(l.trim_matches('`'));
                        out.push('\n');
                    }
                }
                out.truncate(800);
                return Some(out);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BSD_SED: &str = "SED(1)                      General Commands Manual                     SED(1)

NAME
     sed - stream editor

SYNOPSIS
     sed [-Ealnru] command [-I extension] [-i extension] [file ...]

DESCRIPTION
     The following options are available:

     -E      Interpret regular expressions as extended (modern) regular
             expressions rather than basic regular expressions (BRE's).

     -i extension
             Edit files in-place similarly to -I, but treat each file
             independently from other files.  If a zero-length extension is given, no backup will be saved.

     -n      By default, each line of input is echoed to the standard output
             after all of the commands have been applied to it.
";

    const GNU_GREP: &str = "GREP(1)                          User Commands                         GREP(1)

NAME
       grep - print lines that match patterns

OPTIONS
       -i, --ignore-case
              Ignore case distinctions in patterns and input data.

       -r, --recursive
              Read all files under each directory, recursively.

       -v, --invert-match
              Invert the sense of matching, to select non-matching lines.
";

    #[test]
    fn picks_matching_bsd_entries() {
        let words = crate::context::request_words("replace foo with bar in place in config.txt");
        let e = excerpt(BSD_SED, &words, "");
        assert!(e.contains("sed - stream editor"));
        assert!(e.contains("[-i extension]"));
        assert!(e.contains("-i extension Edit files in-place"), "{e}");
        assert!(!e.contains("-n      By default"));
    }

    #[test]
    fn picks_matching_gnu_entries() {
        let words = crate::context::request_words("search recursively ignoring case");
        let e = excerpt(GNU_GREP, &words, "");
        assert!(e.contains("--ignore-case"), "{e}");
        assert!(e.contains("--recursive"), "{e}");
        assert!(!e.contains("--invert-match"), "{e}");
    }

    #[test]
    fn typed_flags_are_explained() {
        let e = excerpt(BSD_SED, &[], "sed -n");
        assert!(e.contains("-n By default, each line"), "{e}");
    }

    #[test]
    fn overstrike_removed() {
        assert_eq!(strip_overstrike("N\u{8}NA\u{8}AM\u{8}ME\u{8}E _\u{8}f_\u{8}i_\u{8}l_\u{8}e"), "NAME file");
    }

    #[test]
    fn commands_from_line_and_prose() {
        assert_eq!(commands_named("finish this", "FOO=1 tar -x | gzip"), vec!["tar", "gzip"]);
        assert_eq!(commands_named("find files bigger than 1 GB", ""), vec!["find"]);
        assert!(!commands_named("kill that ghostty process", "").contains(&"ghostty".to_string()));
        assert!(commands_named("Bro, open a browser searching for salsa", "").contains(&"open".to_string()));
        assert_eq!(commands_named("rsync but skip node_modules", ""), vec!["rsync"]);
        assert!(commands_named("what's using port 3000", "").is_empty());
    }
}
