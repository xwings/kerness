//! An open discussion: three participants, one facilitator, one skill.
//!
//! The `discussion` gameplan has no adversarial structure — the orchestrator
//! decides who speaks and when it has heard enough. What the framework supplies
//! is the same either way; only the Markdown changes.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-... cargo run -p kerness --example discussion
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
        gameplan: "discussion".to_string(),
        topic: "What makes a good programming language?".to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    for (name, model, persona) in [
        (
            "Alice",
            "openai/gpt-4o",
            "Systems programmer who values performance",
        ),
        (
            "Bob",
            "anthropic/claude-sonnet-4",
            "Web developer who values developer experience",
        ),
        (
            "Carol",
            "openai/gpt-4o",
            "Academic who studies programming language theory",
        ),
    ] {
        session.add_agent(Agent {
            persona: Some(persona.to_string()),
            ..Agent::new(name).with_model(model)
        })?;
    }
    session.add_agent(
        Agent::new("Facilitator")
            .with_model("openai/gpt-4o")
            .with_role("orchestrator"),
    )?;

    // The prompt carries one line per skill — the name and the description. The
    // body only arrives if an agent calls the `Skill` tool for it.
    session.add_skill("summarize")?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Rounds:  {}", result.rounds_run);
    println!("Ended on: {}", result.end_reason);
    println!("Summary:  {}", result.summary());
    Ok(())
}
