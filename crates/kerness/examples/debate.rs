//! A debate run from Rust alone — no Python in the process.
//!
//! ```sh
//! OPENAI_API_KEY=sk-... cargo run -p kerness --example debate
//! ```

use std::sync::Arc;

use kerness::agent::{Agent, Role};
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{OpenAiConfig, OpenAiProvider};
use kerness::session::{Session, SessionConfig};

fn main() -> Result<()> {
    let provider = Arc::new(OpenAiProvider::new(OpenAiConfig {
        api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        ..Default::default()
    })?);

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Should the cache be write-through?".to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    session.add_participant(Agent::new("Alice", "gpt-4o"));
    session.add_participant(Agent::new("Bob", "gpt-4o"));
    session.add_orchestrator(Agent {
        role: Role::Orchestrator,
        ..Agent::new("Mod", "gpt-4o")
    })?;

    let result = session.run()?;
    println!("{}", result.summary());
    Ok(())
}
