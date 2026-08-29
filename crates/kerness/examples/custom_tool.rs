//! Giving agents a tool the framework did not write.
//!
//! A tool is a name, a description, a JSON Schema, and a handler. The schema is
//! the contract: arguments are validated against it before the handler is
//! called, so a handler receives what it asked for or is not called at all.
//!
//! Two things worth noticing in the handler below:
//!
//! - It returns `Result`, and an `Err` is *not* a failed run. The dispatcher
//!   turns it into a tool result the model reads and can work around, exactly
//!   like a successful one.
//! - It takes an `actor` — the agent that made the call. A tool that touches
//!   anything per-agent needs it, and a tool that logs wants it.
//!
//! The dialect is not the caller's problem. An OpenAI-backed agent gets native
//! `tool_calls`; a text-only backend gets the same spec rendered into its
//! prompt and its fenced reply parsed back out.
//!
//! ```sh
//! OPENAI_API_KEY=sk-... cargo run -p kerness --example custom_tool
//! ```

use std::sync::Arc;

use kerness::agent::Agent;
use kerness::channel::ConsoleChannel;
use kerness::error::{Error, Result};
use kerness::provider::{OpenAiConfig, OpenAiProvider};
use kerness::session::{Session, SessionConfig};
use kerness::tooling::Arguments;
use serde_json::{json, Value};

/// The prices this example knows. A real one would ask a service.
const PRICES: [(&str, f64); 3] = [("KER", 41.20), ("ACME", 7.05), ("NRTH", 118.75)];

/// Look up one ticker's last price.
fn quote(arguments: &Arguments, actor: &str) -> Result<String> {
    // Present and a string, because the schema said `required` and `string`.
    let ticker = arguments
        .get("ticker")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();

    let Some((_, price)) = PRICES.iter().find(|(name, _)| *name == ticker) else {
        // The model reads this and tries something else. The run continues.
        let known: Vec<&str> = PRICES.iter().map(|(name, _)| *name).collect();
        return Err(Error::Value(format!(
            "No quote for '{ticker}'. Known tickers: {}",
            known.join(", ")
        )));
    };

    println!("[tool] {actor} asked for {ticker}");
    Ok(format!("{ticker} last traded at {price:.2}"))
}

fn quote_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ticker": {
                "type": "string",
                "description": "The ticker symbol, e.g. KER.",
            }
        },
        "required": ["ticker"],
    })
}

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
        gameplan: "debate".to_string(),
        topic: "Is KER fairly priced against ACME and NRTH?".to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    // `Skill` is reserved — the runtime builds one per turn — and a duplicate
    // name is refused, so this returns a `Result` rather than swallowing either.
    session.add_tool(
        "quote",
        "Look up a ticker's last traded price.",
        quote_schema(),
        Arc::new(quote),
    )?;

    session.add_agent(Agent {
        persona: Some("Value investor who checks every number".to_string()),
        ..Agent::new("Alice").with_model("gpt-4o")
    })?;
    session.add_agent(Agent {
        persona: Some("Momentum trader who distrusts fundamentals".to_string()),
        ..Agent::new("Bob").with_model("gpt-4o")
    })?;
    session.add_agent(
        Agent::new("Mod")
            .with_model("gpt-4o")
            .with_role("orchestrator"),
    )?;

    let result = session.run()?;
    println!("\nSummary: {}", result.summary());
    Ok(())
}
