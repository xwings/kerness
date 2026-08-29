//! A research session that keeps notes, and a caller that reads them after.
//!
//! `memory_write` is off by default, which makes a run read-only: the
//! `write_memory` tool is not offered and `@MEMORY:` notes are dropped. Turning
//! it on gives the agents one shared Markdown file that outlives the
//! conversation — the transcript is what was said, the memory is what they
//! decided was worth keeping.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-... cargo run -p kerness --example research
//! ```

use std::sync::Arc;

use kerness::agent::Agent;
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{OpenRouterConfig, OpenRouterProvider};
use kerness::session::{Session, SessionConfig};

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

    let mut session = Session::new(SessionConfig {
        gameplan: "research".to_string(),
        topic: "What are the implications of quantum computing on current \
                cryptographic standards?"
            .to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        // The file is yours: free-form prose, read as empty when absent, and
        // nothing is templated into it on your behalf.
        memory: "research-notes.md".to_string(),
        memory_write: true,
        // Written after every turn, so an interrupted run resumes instead of
        // restarting. Delete it to start the topic over.
        session_file: Some("research-run.json".to_string()),
        ..Default::default()
    })?;

    for (name, model, persona) in [
        ("Dr. Chen", "openai/gpt-4o", "Quantum computing researcher"),
        (
            "Prof. Smith",
            "anthropic/claude-sonnet-4",
            "Cryptography expert",
        ),
        ("Dr. Patel", "openai/gpt-4o", "Cybersecurity policy analyst"),
    ] {
        session.add_agent(Agent {
            persona: Some(persona.to_string()),
            ..Agent::new(name).with_model(model)
        })?;
    }
    session.add_agent(
        Agent::new("Lead Researcher")
            .with_model("openai/gpt-4o")
            .with_role("orchestrator"),
    )?;

    session.add_skill("summarize")?;
    session.add_skill("fact-check")?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Turns:   {}", result.turns_completed);
    println!("Summary: {}", result.summary());

    // Read from the session rather than from a snapshot taken earlier: the file
    // is written during the run, so anything captured before it is stale.
    let notes = session.memory();
    if notes.trim().is_empty() {
        println!("\nThe agents kept no notes.");
    } else {
        println!("\n--- research-notes.md ---\n{notes}");
    }
    Ok(())
}
