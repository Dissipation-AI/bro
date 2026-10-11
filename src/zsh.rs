//! `bro init zsh` and `bro install`.

use crate::config::{Config, home};

const SCRIPT: &str = include_str!("../shell/bro.zsh");
const INSTALL_LINE: &str = "eval \"$(bro init zsh)\"";

/// The widget, with this binary's absolute path and the configured completion key filled in.
pub fn init_script(config: &Config) -> String {
    let bin = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "bro".into());
    SCRIPT.replace("__BRO_BIN__", &shell_quote(&bin)).replace("__BRO_KEY__", &config.complete_key)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Add the init line to `.zshrc` once.
pub fn install() -> Result<(), String> {
    let dir = std::env::var_os("ZDOTDIR").map(std::path::PathBuf::from).unwrap_or_else(home);
    let rc = dir.join(".zshrc");
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == INSTALL_LINE) {
        println!("bro is already in {}.", rc.display());
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# bro: type `bro` and what you want, then Enter. https://github.com/Dissipation-AI/bro\n");
    text.push_str(INSTALL_LINE);
    text.push('\n');
    std::fs::write(&rc, text).map_err(|e| format!("{}: {e}", rc.display()))?;
    println!("Added bro to {}. Open a new tab, or run: source {}", rc.display(), rc.display());
    if crate::config::Config::load().is_ok() && !crate::config::config_path().exists() {
        println!("No model is configured yet; bro will use apfel on localhost. Run `bro config` to choose.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholders_filled() {
        let s = super::init_script(&crate::config::Config::default());
        assert!(!s.contains("__BRO_"));
        assert!(s.contains("bindkey -M emacs '^G' _bro_complete"));
    }
}
