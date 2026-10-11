//! `bro config` (interactive) and `bro config --check`.

use crate::config::{Config, config_path};
use crate::prompt::Message;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

struct Preset {
    name: &'static str,
    endpoint: &'static str,
    model: &'static str,
    api_key_env: &'static str,
    /// Thinking models think for seconds before a one-line answer unless told not to.
    reasoning_effort: &'static str,
    /// Prompt budget. On-device models read about a token per millisecond, so keep it small;
    /// cloud models are fast enough for more context.
    budget: usize,
    note: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "apfel (Apple's on-device model, free, private; macOS 26+)",
        endpoint: "http://localhost:11434/v1",
        model: "apple-foundationmodel",
        api_key_env: "",
        reasoning_effort: "",
        budget: crate::config::APFEL_BUDGET,
        note: "Start it with `brew services start apfel` (or `apfel --serve`). It uses port 11434, like Ollama.",
    },
    Preset {
        name: "Ollama",
        endpoint: "http://localhost:11434/v1",
        model: "qwen3:8b",
        api_key_env: "",
        reasoning_effort: "",
        budget: crate::config::DEFAULT_BUDGET,
        note: "Pull a model first, e.g. `ollama pull qwen3:8b`.",
    },
    Preset {
        name: "llama.cpp server",
        endpoint: "http://localhost:8080/v1",
        model: "default",
        api_key_env: "",
        reasoning_effort: "",
        budget: crate::config::DEFAULT_BUDGET,
        note: "",
    },
    Preset {
        name: "LM Studio",
        endpoint: "http://localhost:1234/v1",
        model: "default",
        api_key_env: "",
        reasoning_effort: "",
        budget: crate::config::DEFAULT_BUDGET,
        note: "",
    },
    Preset {
        name: "Google Gemini (cloud)",
        endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
        model: "gemini-3.5-flash-lite",
        api_key_env: "GEMINI_API_KEY",
        reasoning_effort: "",
        budget: crate::config::DEFAULT_BUDGET,
        note: "Put your key in an environment variable (bro stores only its name).",
    },
    Preset {
        name: "Custom OpenAI-compatible endpoint",
        endpoint: "",
        model: "",
        api_key_env: "",
        reasoning_effort: "",
        budget: crate::config::DEFAULT_BUDGET,
        note: "",
    },
];

const SEARCH_ENGINES: &[(&str, &str)] = &[
    ("Brave", "https://search.brave.com/search?q={query}"),
    ("Google", "https://www.google.com/search?q={query}"),
    ("DuckDuckGo", "https://duckduckgo.com/?q={query}"),
    ("Kagi", "https://kagi.com/search?q={query}"),
];

fn ask(prompt: &str, default: &str) -> Result<String, String> {
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("cancelled".into());
    }
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

fn choose(prompt: &str, options: &[&str], default: usize) -> Result<usize, String> {
    for (i, o) in options.iter().enumerate() {
        println!("  {}. {o}", i + 1);
    }
    loop {
        let answer = ask(prompt, &(default + 1).to_string())?;
        match answer.parse::<usize>() {
            Ok(n) if (1..=options.len()).contains(&n) => return Ok(n - 1),
            _ => println!("Type a number from 1 to {}.", options.len()),
        }
    }
}

pub fn interactive() -> Result<(), String> {
    let mut config = Config::load().unwrap_or_default();
    println!("bro config: where bro sends requests. Saved to {}.\n", config_path().display());
    let names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
    let current = PRESETS.iter().position(|p| p.endpoint == config.endpoint && p.model == config.model).unwrap_or(0);
    let preset = &PRESETS[choose("Model server", &names, current)?];
    if !preset.note.is_empty() {
        println!("{}", preset.note);
    }
    config.endpoint = ask("Endpoint", if preset.endpoint.is_empty() { &config.endpoint } else { preset.endpoint })?;
    config.api_key_env = ask("Environment variable holding the API key (empty for none)", preset.api_key_env)?;
    if !config.api_key_env.is_empty() && config.api_key().is_none() {
        println!("Note: ${} isn't set in this shell. Set it in ~/.zshrc before using bro.", config.api_key_env);
    }
    let models = list_models(&config);
    if !models.is_empty() {
        println!("Models on this server: {}", models.join(", "));
    }
    let default_model = if !preset.model.is_empty() && (models.is_empty() || models.iter().any(|m| m == preset.model)) {
        preset.model.to_string()
    } else {
        models.first().cloned().unwrap_or_else(|| config.model.clone())
    };
    config.model = ask("Model", &default_model)?;
    config.max_context_tokens = Some(preset.budget);
    config.reasoning_effort = preset.reasoning_effort.to_string();
    let engines: Vec<&str> = SEARCH_ENGINES.iter().map(|(n, _)| *n).collect();
    let current_engine = SEARCH_ENGINES.iter().position(|(_, u)| *u == config.search_url).unwrap_or(0);
    config.search_url = SEARCH_ENGINES[choose("Web search", &engines, current_engine)?].1.to_string();
    config.browser = ask("Browser for web searches (\"default\" or an app name)", &config.browser)?;

    if !config.endpoint_is_local() {
        println!("\nWarning: {} is not on this machine or your local network.", config.endpoint);
        println!("Your recent commands, their output and folder listings will be sent there (with secrets redacted).");
    }
    config.save()?;
    println!("\nSaved. Testing…");
    check(&config)
}

/// Models the server lists at `GET /models`, if it supports that.
fn list_models(config: &Config) -> Vec<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
    let mut req = agent.get(&format!("{}/models", config.endpoint.trim_end_matches('/')));
    if let Some(k) = config.api_key() {
        req = req.header("authorization", &format!("Bearer {k}"));
    }
    let Ok(mut resp) = req.call() else { return Vec::new() };
    let Ok(text) = resp.body_mut().read_to_string() else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(|s| s.trim_start_matches("models/").to_string()))
                .take(30)
                .collect()
        })
        .unwrap_or_default()
}

/// Send a tiny request and report latency.
pub fn check(config: &Config) -> Result<(), String> {
    println!("Endpoint: {}  model: {}", config.endpoint, config.model);
    let messages = vec![
        Message { role: "system", content: "Turn the request into one zsh command for macOS. Answer with JSON: {\"command\": \"...\", \"explanation\": \"...\", \"confidence\": \"high\"}".into() },
        Message { role: "user", content: "Request: list the files in this folder, newest first".into() },
    ];
    let started = Instant::now();
    match crate::llm::ask(&messages, config, &mut |_| {}) {
        Ok(a) => {
            println!("Answer in {} ms: {}", started.elapsed().as_millis(), a.command);
            if started.elapsed() > Duration::from_secs(3) {
                println!(
                    "That's slow for a prompt fill-in. A smaller model, or a smaller max_context_tokens, will help."
                );
            }
            Ok(())
        }
        Err(e) => Err(format!("test request failed after {} ms: {e}", started.elapsed().as_millis())),
    }
}
