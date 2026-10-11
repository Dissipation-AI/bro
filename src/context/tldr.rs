//! Examples from tldr pages (https://tldr.sh) for the commands a request or typed line names
//! (`git rebase …`, `ffmpeg …`), choosing the examples that best match the request. Only tools
//! this machine has are indexed.
//!
//! Matching *any* request to tldr examples by shared words was tried and measured: it added noise
//! that cost accuracy on both a small and a mid-size model (evals 2026-10-11), so a page is used
//! only when its command is named.
//!
//! The background refresh (`bro --refresh-cache`) flattens the pages into
//! `~/Library/Caches/bro/tldr.tsv`, one example per line; a request scans that file.

use super::{request_words, stem};
use crate::config::{cache_dir, home};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Rebuilt at most this often (tldr clients update every week or two).
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const PER_PAGE: usize = 2;
/// A line must match at least this well to be shown: about one uncommon word in its description.
const MIN_SCORE: f64 = 6.0;
/// …and at least this fraction of the best match.
const RELATIVE: f64 = 0.6;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Example {
    pub page: String,
    pub description: String,
    pub command: String,
}

fn index_path() -> PathBuf {
    cache_dir().join("tldr.tsv")
}

/// Where installed tldr clients keep their pages: tlrc, tealdeer, the Node client.
fn pages_root() -> Option<PathBuf> {
    let home = home();
    [
        home.join("Library/Caches/tlrc/pages.en"),
        home.join("Library/Caches/tealdeer/tldr-pages/pages.en"),
        home.join("Library/Caches/tealdeer/tldr-pages/pages"),
        home.join(".cache/tealdeer/tldr-pages/pages.en"),
        home.join(".tldr/cache/pages"),
    ]
    .into_iter()
    .find(|p| p.join("common").is_dir())
}

/// `{{path/to/file}}` → `path/to/file`; `{{[-f|--from-file]}}` → `--from-file`.
pub fn fill_placeholders(command: &str) -> String {
    let mut out = String::new();
    let mut rest = command;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let filled = match inner.strip_prefix('[').and_then(|i| i.strip_suffix(']')) {
            Some(alternatives) if alternatives.contains('|') => alternatives.rsplit('|').next().unwrap_or(alternatives),
            _ => inner,
        };
        out.push_str(filled);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Examples in a page: `- description:` followed by `` `command` ``.
pub fn parse_page(text: &str) -> (String, Vec<(String, String)>) {
    let mut summary = String::new();
    let mut examples = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if let Some(s) = line.strip_prefix("> ") {
            if summary.is_empty() && !s.starts_with("More information") {
                summary = s.trim_end_matches('.').to_string();
            }
        } else if let Some(d) = line.strip_prefix("- ") {
            pending = Some(d.trim_end_matches(':').to_string());
        } else if line.starts_with('`')
            && line.ends_with('`')
            && line.len() > 2
            && let Some(d) = pending.take()
        {
            examples.push((d, fill_placeholders(&line[1..line.len() - 1])));
        }
    }
    (summary, examples)
}

fn keys(text: &str) -> String {
    let mut seen = HashSet::new();
    request_words(&text.replace(['-', '/', '.'], " "))
        .iter()
        .map(|w| stem(w))
        .filter(|s| seen.insert(s.clone()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pages whose command is on this machine: `git-commit` counts when `git` is there.
fn installed(page: &str) -> bool {
    super::man::on_path(page) || page.split_once('-').is_some_and(|(tool, _)| super::man::on_path(tool))
}

/// Rebuild `tldr.tsv` when it's missing or a day old. Called from the background refresh.
pub fn refresh() {
    let path = index_path();
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < MAX_AGE);
    if fresh {
        return;
    }
    let Some(root) = pages_root() else { return };
    // macOS pages override the common ones of the same name.
    let mut pages: HashMap<String, PathBuf> = HashMap::new();
    for platform in ["common", "osx"] {
        let Ok(entries) = std::fs::read_dir(root.join(platform)) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if let Some(name) =
                p.file_stem().and_then(|s| s.to_str()).filter(|_| p.extension().is_some_and(|x| x == "md"))
            {
                pages.insert(name.to_string(), p);
            }
        }
    }
    let mut names: Vec<&String> = pages.keys().filter(|n| installed(n)).collect();
    names.sort();
    let mut out = String::new();
    let clean = |s: &str| s.replace(['\t', '\n'], " ");
    for name in names {
        let Ok(text) = std::fs::read_to_string(&pages[name]) else { continue };
        let (summary, examples) = parse_page(&text);
        let page_keys = keys(&format!("{} {summary}", name.replace('-', " ")));
        for (description, command) in examples {
            out.push_str(&format!(
                "{name}\t{}\t{}\t{}\t{}\t{}\n",
                clean(&description),
                clean(&command),
                keys(&description),
                page_keys,
                keys(&command)
            ));
        }
    }
    let tmp = path.with_extension("tsv.tmp");
    if std::fs::create_dir_all(cache_dir()).is_ok() && std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The examples that best match the request, at most `limit`, best first.
///
/// Each request word that an example shares counts by how rare it is among all examples (IDF), so
/// "csv" or "png" outweigh "convert" or "file". A match in the example's description counts
/// double, in the page's name or summary and in the command itself once; naming the command
/// outright adds the most.
pub fn relevant(request: &str, line: &str, limit: usize) -> Vec<Example> {
    let Ok(index) = std::fs::read_to_string(index_path()) else { return Vec::new() };
    let mut wanted: Vec<String> = request_words(&format!("{request} {line}")).iter().map(|w| stem(w)).collect();
    wanted.dedup();
    let named: HashSet<String> = format!("{request} {line}").split_whitespace().map(|w| w.to_lowercase()).collect();
    struct Row<'a> {
        page: &'a str,
        description: &'a str,
        command: &'a str,
        desc: Vec<usize>,
        other: Vec<usize>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut df = vec![0usize; wanted.len()];
    for row in index.lines() {
        let cols: Vec<&str> = row.split('\t').collect();
        let [page, description, command, desc_keys, page_keys, command_keys] = cols[..] else { continue };
        let found = |keys: &str| -> Vec<usize> {
            let set: HashSet<&str> = keys.split(' ').collect();
            (0..wanted.len()).filter(|&i| set.contains(wanted[i].as_str())).collect()
        };
        let desc = found(desc_keys);
        let mut other = found(page_keys);
        other.extend(found(command_keys));
        other.sort_unstable();
        other.dedup();
        for i in desc.iter().chain(&other).collect::<HashSet<_>>() {
            df[*i] += 1;
        }
        // Only pages for a named command (`git-rebase` when "git" is named, too).
        let tool = page.split('-').next().unwrap_or(page);
        if named.contains(page) || named.contains(tool) {
            rows.push(Row { page, description, command, desc, other });
        }
    }
    let total = index.lines().count().max(1) as f64;
    let idf: Vec<f64> = df.iter().map(|&n| (total / (n as f64 + 1.0)).ln().max(0.0)).collect();
    let mut scored: Vec<(f64, usize)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut score =
                r.desc.iter().map(|&w| 2.0 * idf[w]).sum::<f64>() + r.other.iter().map(|&w| idf[w]).sum::<f64>();
            if named.contains(r.page) {
                score += 6.0;
            }
            (score, i)
        })
        .filter(|(score, _)| *score >= MIN_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    // Only examples about as good as the best one; a long tail of weak matches is noise.
    let best = scored.first().map(|s| s.0).unwrap_or(0.0);
    scored.retain(|s| s.0 >= best * RELATIVE);
    let mut per_page: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for (_, i) in scored {
        let r = &rows[i];
        let n = per_page.entry(r.page).or_default();
        if *n >= PER_PAGE {
            continue;
        }
        *n += 1;
        out.push(Example { page: r.page.into(), description: r.description.into(), command: r.command.into() });
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_become_plain_words() {
        assert_eq!(
            fill_placeholders("jq {{[-f|--from-file]}} {{path/to/script.jq}}"),
            "jq --from-file path/to/script.jq"
        );
        assert_eq!(fill_placeholders("wc -w {{path/to/file}}"), "wc -w path/to/file");
        assert_eq!(fill_placeholders("echo {{unclosed"), "echo {{unclosed");
    }

    #[test]
    fn parses_examples_and_summary() {
        let page = "# wc\n\n> Count lines, words, and bytes.\n> More information: <https://x>.\n\n- Count all words in a file:\n\n`wc -w {{path/to/file}}`\n";
        let (summary, examples) = parse_page(page);
        assert_eq!(summary, "Count lines, words, and bytes");
        assert_eq!(examples, vec![("Count all words in a file".to_string(), "wc -w path/to/file".to_string())]);
    }
}
