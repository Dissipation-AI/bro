//! macOS version, CPU architecture, shell and terminal.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct System {
    pub macos: String,
    pub arch: String,
    pub shell: String,
    pub terminal: String,
    /// Which of [`TOOLS`] are installed, as `which` finds them (empty: not looked up).
    pub tools: Vec<String>,
}

/// Tools worth knowing about: better versions of standard ones (rg, fd, eza, bat), and the ones
/// bro's own session view uses (tree).
pub const TOOLS: &[&str] =
    &["tree", "rg", "fd", "fzf", "eza", "bat", "jq", "htop", "btop", "gh", "uv", "conda", "mamba", "docker", "brew"];

pub fn read() -> System {
    System {
        macos: macos_version().unwrap_or_default(),
        arch: std::env::consts::ARCH.replace("aarch64", "arm64"),
        shell: std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(String::from)).unwrap_or_default(),
        terminal: std::env::var("TERM_PROGRAM").unwrap_or_default(),
        tools: TOOLS.iter().filter_map(|t| super::man::which(t)).map(|p| super::folder::tilde(&p)).collect(),
    }
}

/// From SystemVersion.plist (an XML plist), without spawning `sw_vers`.
fn macos_version() -> Option<String> {
    let text = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let after = text.split("<key>ProductVersion</key>").nth(1)?;
    let value = after.split("<string>").nth(1)?.split("</string>").next()?;
    Some(value.trim().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_version() {
        let v = super::macos_version().unwrap();
        assert!(v.chars().next().unwrap().is_ascii_digit(), "{v}");
    }
}
