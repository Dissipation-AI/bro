//! Installed apps, projects under `~` and the default browser, cached in
//! `~/Library/Caches/bro/index.json`. Requests only read the cache; when it's older than ten
//! minutes a detached `bro --refresh-cache` rebuilds it, so no request waits on a filesystem walk.

use crate::config::{cache_dir, home};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_AGE: Duration = Duration::from_secs(600);
const MAX_PROJECTS: usize = 3_000;
/// Folders macOS guards with privacy prompts (TCC). Walking them from a background process
/// would make the terminal ask for access, so they're left out.
const PROTECTED: &[&str] =
    &["Desktop", "Documents", "Downloads", "Library", "Pictures", "Movies", "Music", "Public", "Applications"];
const SKIP: &[&str] =
    &["node_modules", "target", "vendor", "venv", "build", "dist", "__pycache__", "Pods", "DerivedData"];

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub refreshed_unix: u64,
    pub apps: Vec<String>,
    /// `~/…` paths: top-level and second-level folders, and git repositories up to three deep.
    pub projects: Vec<String>,
    /// Filled per request for matching projects; never stored.
    #[serde(skip)]
    pub programs: Vec<String>,
    pub default_browser: Option<String>,
}

fn index_path() -> PathBuf {
    cache_dir().join("index.json")
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The parts of the index that relate to the request. Starts a background refresh if stale.
pub fn load_relevant(words: &[String]) -> Option<Index> {
    let index: Option<Index> = std::fs::read(index_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let stale = index.as_ref().is_none_or(|i| now_unix().saturating_sub(i.refreshed_unix) > MAX_AGE.as_secs());
    if stale {
        start_refresh();
    }
    let index = index?;
    let wants_browser = words.iter().any(|w| {
        ["browser", "web", "search", "google", "website", "url", "tab", "safari", "chrome"].contains(&w.as_str())
    });
    // Everything, most relevant first; the prompt trims each list to its size budget.
    let apps =
        by_relevance(&index.apps, |a| super::relevant(a, words) as u8 * 2 + (wants_browser && is_browser(a)) as u8);
    let projects = by_relevance(&index.projects, |p| super::relevant(last_component(p), words) as u8);
    let mut programs: Vec<String> = Vec::new();
    for p in index.projects.iter().filter(|p| super::relevant(last_component(p), words)).take(6) {
        programs.extend(project_entries(p));
    }
    Some(Index {
        refreshed_unix: index.refreshed_unix,
        apps,
        projects,
        programs,
        default_browser: index.default_browser,
    })
}

/// Stable sort, highest score first.
fn by_relevance(items: &[String], score: impl Fn(&str) -> u8) -> Vec<String> {
    let mut v: Vec<(u8, &String)> = items.iter().map(|i| (score(i), i)).collect();
    v.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    v.into_iter().map(|(_, i)| i.clone()).collect()
}

fn last_component(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// For a matched project, what's directly runnable in it: executables in its folder and `bin/`.
fn project_entries(project: &str) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let base = expand(project);
    let mut found = Vec::new();
    for dir in [base.clone(), base.join("bin")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten().take(200) {
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 && !name.starts_with('.') {
                found.push(crate::context::folder::tilde(&dir.join(&name)));
            }
        }
    }
    found.sort();
    found.truncate(8);
    found
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

pub fn is_browser(app: &str) -> bool {
    [
        "Safari",
        "Chrome",
        "Firefox",
        "Brave",
        "Arc",
        "Edge",
        "Opera",
        "Vivaldi",
        "Orion",
        "Zen",
        "Browser",
        "Chromium",
        "DuckDuckGo",
    ]
    .iter()
    .any(|b| app.contains(b))
}

/// Launch a detached refresh unless one started in the last two minutes.
fn start_refresh() {
    let dir = cache_dir();
    let lock = dir.join("refresh.lock");
    if let Ok(meta) = std::fs::metadata(&lock)
        && meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(120))
    {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() || std::fs::write(&lock, b"").is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    use std::os::unix::process::CommandExt;
    let _ = std::process::Command::new(exe)
        .arg("--refresh-cache")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// Rebuild the index. Runs in the background process started by `start_refresh`.
pub fn refresh() -> Result<(), String> {
    let index = Index {
        refreshed_unix: now_unix(),
        apps: apps(),
        projects: projects(),
        programs: Vec::new(),
        default_browser: default_browser(),
    };
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("index.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&index).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, index_path()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir.join("refresh.lock"));
    Ok(())
}

fn apps() -> Vec<String> {
    let home_apps = home().join("Applications");
    let roots: Vec<PathBuf> = vec![
        "/Applications".into(),
        "/Applications/Utilities".into(),
        "/System/Applications".into(),
        "/System/Applications/Utilities".into(),
        home_apps.clone(),
        home_apps.join("Chrome Apps.localized"),
    ];
    let mut apps = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(app) = name.strip_suffix(".app") {
                apps.push(app.to_string());
            }
        }
    }
    apps.sort();
    apps.dedup();
    apps
}

fn projects() -> Vec<String> {
    let home = home();
    let mut found = Vec::new();
    walk(&home, 1, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<String>) {
    if depth > 3 || found.len() >= MAX_PROJECTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) || (depth == 1 && PROTECTED.contains(&name.as_str()))
        {
            continue;
        }
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let is_repo = path.join(".git").exists();
        if depth <= 2 || is_repo {
            found.push(crate::context::folder::tilde(&path));
        }
        if !is_repo {
            walk(&path, depth + 1, found);
        }
    }
}

/// The app that opens https links, from LaunchServices preferences.
fn default_browser() -> Option<String> {
    let plist = home().join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(&plist)
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let handler = json["LSHandlers"].as_array()?.iter().find(|h| h["LSHandlerURLScheme"] == "https")?;
    let id = handler["LSHandlerRoleAll"].as_str()?.to_lowercase();
    let known = [
        ("com.apple.safari", "Safari"),
        ("com.google.chrome", "Google Chrome"),
        ("com.brave.browser", "Brave Browser"),
        ("org.mozilla.firefox", "Firefox"),
        ("company.thebrowser.browser", "Arc"),
        ("com.microsoft.edgemac", "Microsoft Edge"),
        ("com.vivaldi.vivaldi", "Vivaldi"),
        ("com.kagi.kagimacos", "Orion"),
    ];
    Some(known.iter().find(|(k, _)| *k == id).map(|(_, v)| v.to_string()).unwrap_or(id))
}
