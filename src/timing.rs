//! Stage timings for `--timings`, measured from process start.

use std::time::{Duration, Instant};

pub struct Timings {
    start: Instant,
    enabled: bool,
    marks: Vec<(String, Duration)>,
    sources: Vec<(String, Duration)>,
}

impl Timings {
    pub fn new(enabled: bool) -> Timings {
        Timings { start: Instant::now(), enabled, marks: Vec::new(), sources: Vec::new() }
    }

    /// Time since start at which a stage finished.
    pub fn mark(&mut self, stage: &str) {
        if self.enabled {
            self.marks.push((stage.to_string(), self.start.elapsed()));
        }
    }

    /// How long one context source took on its own thread.
    pub fn record(&mut self, source: &str, took: Duration) {
        if self.enabled {
            self.sources.push((source.to_string(), took));
        }
    }

    pub fn print(&self) {
        if !self.enabled {
            return;
        }
        eprintln!("bro timings (milliseconds since start):");
        for (stage, at) in &self.marks {
            eprintln!("  {:>8.2}  {stage}", at.as_secs_f64() * 1000.0);
        }
        if !self.sources.is_empty() {
            eprintln!("context sources (milliseconds each, in parallel):");
            for (source, took) in &self.sources {
                eprintln!("  {:>8.2}  {source}", took.as_secs_f64() * 1000.0);
            }
        }
    }
}
