//! Current folder, its listing, and git branch and status.

use super::Git;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const LISTED: usize = 50;

/// The folder (with `~` for home), its entries two levels deep (hidden files left out,
/// directories marked with `/`), and how many entries its top level has.
pub fn read() -> (String, Vec<String>, usize) {
    let Ok(cwd) = std::env::current_dir() else { return (String::new(), Vec::new(), 0) };
    let (files, total) = list(&cwd);
    (tilde(&cwd), files, total)
}

pub fn tilde(path: &Path) -> String {
    let home = crate::config::home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Entries two levels deep, like `tree -L 2`: paths relative to `dir`, directories ending in `/`.
/// Folders full of dependencies or build output are listed but not opened. Returns the entries
/// (first [`LISTED`] at the top level, a few inside each folder) and the top-level total.
fn list(dir: &Path) -> (Vec<String>, usize) {
    let (top, total) = entries(dir, 2_000);
    let mut out = Vec::new();
    for name in top.into_iter().take(LISTED) {
        let open = name.ends_with('/') && !NOT_OPENED.contains(&name.trim_end_matches('/'));
        out.push(name.clone());
        if open {
            let (inner, inner_total) = entries(&dir.join(name.trim_end_matches('/')), 200);
            let shown = inner.len().min(INSIDE);
            out.extend(inner.into_iter().take(INSIDE).map(|n| format!("{name}{n}")));
            if inner_total > shown {
                out.push(format!("{name}… {} more", inner_total - shown));
            }
        }
    }
    (out, total)
}

/// Entries inside a folder shown by `tree -L 2`.
const INSIDE: usize = 8;
const NOT_OPENED: &[&str] =
    &["node_modules", "target", "build", "dist", "venv", ".venv", "__pycache__", "vendor", "Pods", "DerivedData"];

/// One folder's visible entries, sorted like `ls`, directories marked with `/`; reads at most
/// `max` and also returns the total.
fn entries(dir: &Path, max: usize) -> (Vec<String>, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), 0) };
    let mut names = Vec::new();
    let mut total = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        total += 1;
        if names.len() < max {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push(if is_dir { format!("{name}/") } else { name });
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    (names, total)
}

fn repo() -> Option<gix::Repository> {
    gix::discover(std::env::current_dir().ok()?).ok()
}

/// The branch, read in-process with gix (no `git` spawn).
pub fn git() -> Option<Git> {
    let repo = repo()?;
    let branch = match repo.head_name().ok()? {
        Some(name) => name.shorten().to_string(),
        None => match repo.head_id() {
            Ok(id) => format!("detached at {}", id.shorten_or_id()),
            Err(_) => "no commits yet".into(),
        },
    };
    Some(Git { branch, ..Git::default() })
}

/// Short status, e.g. "2 staged, 1 untracked", and the first changed paths in `git status -s`
/// form (` M src/main.rs`). A separate source from the branch: in a large repository it can
/// outlast the time limit, and then only this is missing. `stop` is set at the deadline, and gix
/// gives up.
pub fn git_status(stop: Arc<AtomicBool>) -> Option<(String, Vec<String>)> {
    let repo = repo()?;
    let iter = repo
        .status(gix::progress::Discard)
        .ok()?
        .untracked_files(gix::status::UntrackedFiles::Collapsed)
        .index_worktree_submodules(None)
        .should_interrupt_owned(stop.clone())
        .into_iter(None)
        .ok()?;
    let mut counts = Counts::default();
    let mut changes = Vec::new();
    for item in iter {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let item = item.ok()?;
        let code = match &item {
            gix::status::Item::TreeIndex(change) => {
                counts.staged += 1;
                use gix::diff::index::ChangeRef;
                match change {
                    ChangeRef::Addition { .. } => "A ",
                    ChangeRef::Deletion { .. } => "D ",
                    _ => "M ",
                }
            }
            gix::status::Item::IndexWorktree(item) => {
                use gix::status::index_worktree::iter::Summary;
                match item.summary() {
                    Some(Summary::Added) => {
                        counts.untracked += 1;
                        "??"
                    }
                    Some(Summary::Conflict) => {
                        counts.conflicted += 1;
                        "UU"
                    }
                    Some(Summary::Removed) => {
                        counts.modified += 1;
                        " D"
                    }
                    Some(_) => {
                        counts.modified += 1;
                        " M"
                    }
                    None => continue,
                }
            }
        };
        if changes.len() < CHANGES_LISTED {
            changes.push(format!("{code} {}", item.location()));
        }
    }
    Some((counts.summary(), changes))
}

/// Changed paths listed, like the start of `git status -s`.
const CHANGES_LISTED: usize = 15;

#[derive(Default)]
struct Counts {
    staged: usize,
    modified: usize,
    untracked: usize,
    conflicted: usize,
}

impl Counts {
    /// e.g. "2 staged, 3 modified, 1 untracked", or "clean".
    fn summary(&self) -> String {
        let parts: Vec<String> = [
            (self.staged, "staged"),
            (self.modified, "modified"),
            (self.untracked, "untracked"),
            (self.conflicted, "conflicted"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        if parts.is_empty() { "clean".into() } else { parts.join(", ") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_summary() {
        assert_eq!(Counts::default().summary(), "clean");
        let c = Counts { staged: 2, modified: 2, untracked: 1, conflicted: 1 };
        assert_eq!(c.summary(), "2 staged, 2 modified, 1 untracked, 1 conflicted");
    }
}
