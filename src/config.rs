//! `~/.config/bro/config.toml`. Every field has a default, so a missing file means apfel on
//! localhost.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// OpenAI-compatible base URL, without `/chat/completions`.
    pub endpoint: String,
    pub model: String,
    /// Name of an environment variable holding the API key, never the key itself.
    pub api_key_env: String,
    pub timeout_ms: u64,
    pub temperature: f32,
    /// Prompt budget in tokens. Unset: 2,500 for apfel (whose window is small, and which reads
    /// about a token per millisecond), 6,000 otherwise. See [`Config::budget`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    /// Ask for JSON constrained by a schema (`response_format`). Unset: yes, except for apfel,
    /// whose constrained decoding ends a string at the first quote inside it (`-name "build"`
    /// comes back as `-name `). Servers that reject it are asked again without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<bool>,
    /// Sent as `reasoning_effort` when set (e.g. "minimal" for thinking models, which otherwise
    /// think for seconds before a one-line answer).
    pub reasoning_effort: String,
    pub search_url: String,
    /// `default`, or an app name such as `Brave Browser`.
    pub browser: String,
    /// The key that sends the current line to bro, in `bindkey` notation.
    pub complete_key: String,
    pub history_examples: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            endpoint: "http://localhost:11434/v1".into(),
            model: "apple-foundationmodel".into(),
            api_key_env: String::new(),
            timeout_ms: 15_000,
            temperature: 0.0,
            max_context_tokens: None,
            json_schema: None,
            reasoning_effort: String::new(),
            search_url: "https://search.brave.com/search?q={query}".into(),
            browser: "default".into(),
            complete_key: "^G".into(),
            history_examples: 20,
        }
    }
}

/// Apple's on-device model, as served by apfel.
pub const APFEL_MODEL: &str = "apple-foundationmodel";
pub const DEFAULT_BUDGET: usize = 6_000;
pub const APFEL_BUDGET: usize = 2_500;
/// At or below this budget, sources are trimmed hard (SPEC: "With a 4K-context model…").
pub const SMALL_BUDGET: usize = 3_000;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("BRO_CONFIG_DIR") {
        return PathBuf::from(dir).join("config.toml");
    }
    home().join(".config/bro/config.toml")
}

/// `~/Library/Application Support/bro/`: the learning history.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BRO_DATA_DIR") {
        return PathBuf::from(dir);
    }
    home().join("Library/Application Support/bro")
}

/// `~/Library/Caches/bro/`: app and project index, rendered man pages.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BRO_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    home().join("Library/Caches/bro")
}

impl Config {
    pub fn load() -> Result<Config, String> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn budget(&self) -> usize {
        self.max_context_tokens.unwrap_or(if self.model == APFEL_MODEL { APFEL_BUDGET } else { DEFAULT_BUDGET })
    }

    /// A small window: last command output capped at ~20 lines, top 5 processes, 5 examples.
    pub fn small(&self) -> bool {
        self.budget() <= SMALL_BUDGET
    }

    pub fn wants_json_schema(&self) -> bool {
        self.json_schema.unwrap_or(self.model != APFEL_MODEL)
    }

    pub fn api_key(&self) -> Option<String> {
        if self.api_key_env.is_empty() {
            return None;
        }
        std::env::var(&self.api_key_env).ok().filter(|k| !k.is_empty())
    }

    /// True when requests stay on this machine or the local network.
    pub fn endpoint_is_local(&self) -> bool {
        let rest = self.endpoint.split("://").nth(1).unwrap_or(&self.endpoint);
        let host = rest.split(['/', '?']).next().unwrap_or("");
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host),
        };
        if host == "localhost" || host.ends_with(".local") || host == "::1" {
            return true;
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoints() {
        let local = |e: &str| Config { endpoint: e.into(), ..Config::default() }.endpoint_is_local();
        assert!(local("http://localhost:11434/v1"));
        assert!(local("http://127.0.0.1:8080/v1"));
        assert!(local("http://192.168.1.20:8080/v1"));
        assert!(local("http://[::1]:8080/v1"));
        assert!(local("http://studio.local:1234/v1"));
        assert!(!local("https://generativelanguage.googleapis.com/v1beta/openai"));
        assert!(!local("https://api.openai.com/v1"));
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let c: Config = toml::from_str("model = \"qwen3-8b\"").unwrap();
        assert_eq!(c.model, "qwen3-8b");
        assert_eq!(c.endpoint, Config::default().endpoint);
        assert_eq!(c.budget(), DEFAULT_BUDGET);
        assert!(c.wants_json_schema());
        let apfel = Config::default();
        assert_eq!(apfel.budget(), APFEL_BUDGET);
        assert!(apfel.small() && !apfel.wants_json_schema());
        let c: Config = toml::from_str("max_context_tokens = 4000").unwrap();
        assert_eq!(c.budget(), 4000);
    }
}
