//! Sending a session's output somewhere the framework has never heard of.
//!
//! Kerness ships a console, a file, a log and a fan-out. It ships no Slack, no
//! Telegram, no webhook — a chat transport is an interface choice belonging to
//! the program using the framework, not to the framework. What that costs is
//! the two methods below.
//!
//! This one writes newline-delimited JSON, which is what a log pipeline wants
//! and no built-in produces. Note that it *reports* a write failure rather than
//! returning it: inside a [`MultiChannel`] the error is logged and the fan-out
//! continues anyway, and on its own a full disk on the notification path should
//! not abort a paid-for run mid-turn. A channel whose delivery is the point —
//! one a human is waiting on — should return the error instead.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-... cargo run -p kerness --example custom_channel
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kerness::agent::{Agent, Role};
use kerness::channel::{Channel, ConsoleChannel, MultiChannel};
use kerness::error::Result;
use kerness::provider::{OpenRouterConfig, OpenRouterProvider};
use kerness::session::{Session, SessionConfig};
use serde_json::json;

/// Appends one JSON object per message.
///
/// The lock is not for the framework's sake — a session runs on one thread and
/// nothing here is concurrent — but because [`Channel`] requires `Sync`, and a
/// channel that a caller also writes to from elsewhere should not interleave
/// half-written lines.
struct JsonLinesChannel {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonLinesChannel {
    fn new(path: impl Into<PathBuf>) -> JsonLinesChannel {
        JsonLinesChannel {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn append(&self, sender: &str, message: &str) {
        let _held = self.lock.lock().expect("channel lock");
        let line = json!({"sender": sender, "text": message});
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| writeln!(file, "{line}"));
        if let Err(err) = written {
            eprintln!("[jsonl] delivery failed, continuing: {err}");
        }
    }
}

impl Channel for JsonLinesChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        self.append(sender, message);
        Ok(())
    }

    fn send_system(&self, message: &str) -> Result<()> {
        self.append("system", message);
        Ok(())
    }

    /// Named for `MultiChannel`'s failure log, which is the only reader.
    fn type_name(&self) -> String {
        "JsonLinesChannel".to_string()
    }
}

fn main() -> Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("Set OPENROUTER_API_KEY first.");
        std::process::exit(1);
    }

    let provider = Arc::new(OpenRouterProvider::new(OpenRouterConfig {
        api_key,
        ..Default::default()
    }));

    // Every message goes to both. One failing does not stop the other, and does
    // not stop the run.
    let channel = Arc::new(MultiChannel::new(vec![
        Arc::new(ConsoleChannel::default()),
        Arc::new(JsonLinesChannel::new("transcript.jsonl")),
    ]));

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Should the cache be write-through?".to_string(),
        provider: Some(provider),
        channel: Some(channel),
        ..Default::default()
    })?;

    session.add_participant(Agent::new("Alice", "openai/gpt-4o"));
    session.add_participant(Agent::new("Bob", "anthropic/claude-sonnet-4"));
    session.add_orchestrator(Agent {
        role: Role::Orchestrator,
        ..Agent::new("Mod", "openai/gpt-4o")
    })?;

    let result = session.run()?;
    println!("\nSummary: {}", result.summary());
    println!("Transcript written to transcript.jsonl");
    Ok(())
}
