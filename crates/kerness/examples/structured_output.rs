//! A closing turn that has to come back as a named shape, twice over.
//!
//! Two mechanisms, at different layers, and they are worth telling apart:
//!
//! - The gameplan's `result:` block is the framework's. It is enforced against
//!   any backend, and it is what fills [`SessionResult::fields`].
//! - `output_schema` is OpenAI's. It is sent with the request and the endpoint
//!   refuses to answer outside it.
//!
//! The Python bindings derive the second from a `pydantic` model. A Rust caller
//! writes the JSON Schema directly, which is what the bindings do for it.
//!
//! ```sh
//! OPENAI_API_KEY=sk-... cargo run -p kerness --example structured_output
//! ```

use std::sync::Arc;

use kerness::agent::{Agent, Role};
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{OpenAiConfig, OpenAiProvider};
use kerness::session::{Session, SessionConfig};
use serde_json::json;

/// The shape the orchestrator's closing turn must come back in.
fn bid_decision() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "vendor_name": {"type": "string"},
            "pass_gate": {
                "type": "boolean",
                "description": "Whether this vendor enters the final round.",
            },
            "score": {"type": "integer", "description": "0-100 overall bid score."},
            "key_risks": {"type": "array", "items": {"type": "string"}},
            "rationale": {"type": "string"},
        },
        "required": ["vendor_name", "pass_gate", "score", "key_risks", "rationale"],
    })
}

fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("Set OPENAI_API_KEY first.");
        std::process::exit(1);
    }

    // `strict_json_schema` rewrites the schema into OpenAI's strict subset —
    // every property required, no additional properties — rather than making
    // the caller hand-write that dialect.
    let structured = Arc::new(OpenAiProvider::new(OpenAiConfig {
        api_key: api_key.clone(),
        output_schema: Some(bid_decision()),
        output_schema_name: "bid_decision".to_string(),
        strict_json_schema: true,
        ..Default::default()
    })?);
    // The participants argue in prose. Only the turn that has to be machine-read
    // is pinned to a schema.
    let prose = Arc::new(OpenAiProvider::new(OpenAiConfig {
        api_key,
        ..Default::default()
    })?);

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Should Northwind Systems pass the bid gate for the depot \
                rebuild? Their price is under budget and their delivery record \
                is uneven."
            .to_string(),
        provider: Some(prose),
        channel: Some(Arc::new(ConsoleChannel::default())),
        ..Default::default()
    })?;

    session.add_participant(Agent {
        persona: "Procurement officer weighing price against delivery risk".to_string(),
        ..Agent::new("Buyer", "gpt-4o")
    });
    session.add_participant(Agent {
        persona: "Engineering lead who has to live with the delivery".to_string(),
        ..Agent::new("Engineer", "gpt-4o")
    });
    session.add_orchestrator(Agent {
        role: Role::Orchestrator,
        provider: Some(structured),
        ..Agent::new("Chair", "gpt-4o")
    })?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Consensus: {}", result.consensus_reached);
    for (name, value) in &result.fields {
        println!("{name}: {value}");
    }
    Ok(())
}
