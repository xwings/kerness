//! Research that reaches outside the process, and the boundary that lets it.
//!
//! The access policy is default-deny: nothing runs, nothing is read, and the
//! refusal an agent gets back names what would have to change. Widening it is
//! the caller's decision and is made explicitly — here, one program, by regex.
//!
//! This example needs the `agent-browser` CLI on `PATH`. Without it the run
//! still works: the tool calls fail, the agents are told so, and they answer
//! from what they already know.
//!
//! ```sh
//! OPENAI_API_KEY=sk-... cargo run -p kerness --example web_research
//! ```

use std::sync::Arc;

use kerness::agent::{Agent, Role};
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{OpenAiConfig, OpenAiProvider};
use kerness::session::{Session, SessionConfig};

fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("Set OPENAI_API_KEY first.");
        std::process::exit(1);
    }

    let provider = Arc::new(OpenAiProvider::new(OpenAiConfig {
        api_key,
        ..Default::default()
    })?);

    let mut session = Session::new(SessionConfig {
        gameplan: "research".to_string(),
        topic: "Sub-agents: definition and current development status.\n\n\
                Use the agent-browser skill to guide your research. Open two or \
                three sources and cite each with its page title and full URL."
            .to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    // One program, matched at the start of the command line. `set_exec`
    // replaces the pattern list rather than appending to it, so the widening
    // stays revocable: passing an empty vector closes it again.
    session.set_exec(vec![r"^agent-browser\b".to_string()]);

    for (name, persona) in [
        ("Alex", "Systems researcher"),
        ("Bo", "AI product analyst"),
        ("Chen", "ML engineer"),
    ] {
        session.add_participant(Agent {
            persona: persona.to_string(),
            ..Agent::new(name, "gpt-4o")
        });
    }
    session.add_orchestrator(Agent {
        role: Role::Orchestrator,
        ..Agent::new("Lead", "gpt-4o")
    })?;

    session.add_skill("agent-browser")?;
    session.add_skill("fact-check")?;
    session.add_skill("summarize")?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Turns:   {}", result.turns_completed);
    println!("Summary: {}", result.summary());
    Ok(())
}
