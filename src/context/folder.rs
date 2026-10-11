//! Current folder, its listing, and git branch and status.

use super::Git;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const LISTED: usize = 50;

/// The folder (with `~` for home), its first entries like `ls` (hidden files left out,
/// directories marked with `/`), and how many entries there are in total.
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

fn list(dir: &Path) -> (Vec<String>, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), 0) };
    let mut names = Vec::new();
    let mut total = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        total += 1;
        if names.len() < 2_000 {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push(if is_dir { format!("{name}/") } else { name });
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.truncate(LISTED);
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
    Some(Git { branch, status: None })
}

/// Short status, e.g. "2 staged, 1 untracked". A separate source from the branch: in a large
/// repository it can outlast the time limit, and then only this line is missing. `stop` is set
/// at the deadline, and gix gives up.
pub fn git_status(stop: Arc<AtomicBool>) -> Option<String> {
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
    for item in iter {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match item.ok()? {
            gix::status::Item::TreeIndex(_) => counts.staged += 1,
            gix::status::Item::IndexWorktree(item) => {
                use gix::status::index_worktree::iter::Summary;
                match item.summary() {
                    Some(Summary::Added) => counts.untracked += 1,
                    Some(Summary::Conflict) => counts.conflicted += 1,
                    Some(_) => counts.modified += 1,
                    None => {}
                }
            }
        }
    }
    Some(counts.summary())
}

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
