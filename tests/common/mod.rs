//! Shared by the integration tests: a fake OpenAI-compatible server and a private HOME.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct FakeServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
}

/// Serve `answer` as the model's message content to every request, after `delay`, streamed as
/// SSE in small chunks (as real servers do when asked to stream).
pub fn serve(answer: &str, delay: Duration) -> FakeServer {
    serve_with(answer, delay, Some(Duration::ZERO))
}

/// As [`serve`], with `chunk_delay` between chunks; `None` answers with one ordinary JSON
/// completion instead of a stream, like servers that ignore `stream`.
pub fn serve_with(answer: &str, delay: Duration, chunk_delay: Option<Duration>) -> FakeServer {
    serve_full(answer, delay, chunk_delay, false)
}

/// Like a server without structured output: a request with `response_format` gets a 400.
pub fn serve_rejecting_schema(answer: &str) -> FakeServer {
    serve_full(answer, Duration::ZERO, Some(Duration::ZERO), true)
}

fn serve_full(answer: &str, delay: Duration, chunk_delay: Option<Duration>, reject_schema: bool) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let answer = answer.to_string();
    let body = serde_json::json!({
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": answer}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut request = vec![0; length];
            let _ = reader.read_exact(&mut request);
            let request = String::from_utf8_lossy(&request).into_owned();
            let rejected = reject_schema && request.contains("response_format");
            seen.lock().unwrap().push(request);
            if rejected {
                let body = r#"{"error": {"message": "Unknown field: response_format"}}"#;
                let _ = stream.write_all(
                    format!("HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes(),
                );
                continue;
            }
            std::thread::sleep(delay);
            if let Some(gap) = chunk_delay {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
                let chars: Vec<char> = answer.chars().collect();
                for piece in chars.chunks(4) {
                    let piece: String = piece.iter().collect();
                    let chunk = serde_json::json!({"choices": [{"index": 0, "delta": {"content": piece}}]});
                    let _ = stream.write_all(format!("data: {chunk}\n\n").as_bytes());
                    let _ = stream.flush();
                    std::thread::sleep(gap);
                }
                let _ = stream.write_all(b"data: [DONE]\n\n");
                continue;
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    FakeServer { url, requests }
}

/// A private HOME with config, data and cache directories, so tests never touch the real ones.
pub struct Sandbox {
    pub home: PathBuf,
}

impl Sandbox {
    pub fn new(name: &str, endpoint: &str) -> Sandbox {
        let home = std::env::temp_dir().join(format!("bro-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/bro")).unwrap();
        std::fs::write(
            home.join(".config/bro/config.toml"),
            format!("endpoint = \"{endpoint}\"\nmodel = \"test-model\"\ntimeout_ms = 5000\n"),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("work")).unwrap();
        std::fs::write(home.join("work/notes.txt"), "hello").unwrap();
        Sandbox { home }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bro"));
        c.env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", "/bin/zsh")
            .env("HISTFILE", self.home.join(".zsh_history"))
            .current_dir(self.home.join("work"))
            .stdin(Stdio::null());
        c
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}
