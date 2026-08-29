//! A debate run from Rust alone — no Python in the process.
//!
//! Shows per-agent providers: an agent's own backend wins over the session's,
//! so one debate can be argued by three different vendors' models. The session
//! provider is the fallback for whoever brings none.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-... cargo run -p kerness --example debate
//! # optionally, to give Alice and Bob backends of their own:
//! OPENAI_API_KEY=sk-... ANTHROPIC_API_KEY=sk-... cargo run -p kerness --example debate
//! ```
//!
//! For a run that needs no key at all, see `offline_debate`.

use std::sync::Arc;

use kerness::agent::Agent;
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{
    ClaudeConfig, ClaudeCredential, ClaudeProvider, OpenAiConfig, OpenAiProvider, OpenRouterConfig,
    OpenRouterProvider, Provider,
};
use kerness::session::{Session, SessionConfig};

/// A backend for *agent*, or `None` when its key is not in the environment.
///
/// Returning `None` rather than an empty-keyed provider matters: an agent with
/// no provider of its own falls back to the session's, which is the behaviour
/// being demonstrated. A provider built around an empty key would instead fail
/// at the first request.
fn keyed(
    variable: &str,
    build: impl FnOnce(String) -> Result<Arc<dyn Provider>>,
) -> Option<Arc<dyn Provider>> {
    let key = std::env::var(variable).unwrap_or_default();
    if key.is_empty() {
        println!("({variable} is unset — that agent will use the session provider.)");
        return None;
    }
    build(key).ok()
}

fn main() -> Result<()> {
    let router_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if router_key.is_empty() {
        eprintln!("Set OPENROUTER_API_KEY first. It backs whichever agents bring no provider.");
        std::process::exit(1);
    }

    let session_provider = Arc::new(OpenRouterProvider::new(OpenRouterConfig {
        api_key: router_key,
        ..Default::default()
    }));

    let alice = keyed("OPENAI_API_KEY", |api_key| {
        Ok(Arc::new(OpenAiProvider::new(OpenAiConfig {
            api_key,
            ..Default::default()
        })?))
    });
    let bob = keyed("ANTHROPIC_API_KEY", |api_key| {
        Ok(Arc::new(ClaudeProvider::new(ClaudeConfig {
            credential: ClaudeCredential::ApiKey(api_key),
            ..Default::default()
        })))
    });

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Should the cache be write-through?".to_string(),
        provider: Some(session_provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    // The model name belongs to whichever provider the agent ends up calling,
    // so it changes with the backend rather than staying fixed. An agent that
    // brings its own provider has to name its own model for exactly that
    // reason: the session's would name a model on the session's backend.
    session.add_agent(Agent {
        persona: Some("Pragmatic engineer".to_string()),
        provider: alice.clone(),
        ..Agent::new("Alice").with_model(if alice.is_some() {
            "gpt-4o"
        } else {
            "openai/gpt-4o"
        })
    })?;
    session.add_agent(Agent {
        persona: Some("Devil's advocate".to_string()),
        provider: bob.clone(),
        ..Agent::new("Bob").with_model(if bob.is_some() {
            "claude-sonnet-4-20250514"
        } else {
            "anthropic/claude-sonnet-4"
        })
    })?;
    session.add_agent(
        Agent::new("Mod")
            .with_model("openai/gpt-4o")
            .with_role("orchestrator"),
    )?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Turns:     {}", result.turns_completed);
    println!("Consensus: {}", result.consensus_reached);
    println!("Summary:   {}", result.summary());
    Ok(())
}
