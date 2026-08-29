//! A whole session, start to finish, with no API key and no network.
//!
//! Every other example needs a credential before it does anything, which makes
//! them a poor first thing to run from a clean clone. This one implements
//! [`Provider`] over a list of canned replies, so the loop, the routing, the
//! terminator, the closing turn and the declared result fields are all real —
//! only the model is not.
//!
//! It is also the smoke test CI runs: a change that breaks the run breaks this
//! before it reaches anyone's key.
//!
//! ```sh
//! cargo run -p kerness --example offline_debate
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kerness::agent::{Agent, Role};
use kerness::channel::ConsoleChannel;
use kerness::error::Result;
use kerness::provider::{Provider, ProviderBase, ProviderResponse, ReasoningEffort};
use kerness::session::{Session, SessionConfig};
use kerness::tooling::ToolSpec;
use serde_json::Value;

/// A backend that reads from a script instead of a socket.
///
/// The `purpose` a session hands to `chat_with_retries` says what the call is
/// for — `orchestrator turn`, `turn from Alice`, `final summary` — so replies
/// can be keyed on it rather than on a bare counter. Routing has to alternate,
/// though: a round closes only once every participant has spoken, so the
/// orchestrator gets a sequence and everyone else gets one answer.
struct Canned {
    base: ProviderBase,
    routes: Vec<&'static str>,
    next_route: AtomicUsize,
}

impl Canned {
    fn new(routes: Vec<&'static str>) -> Arc<Canned> {
        Arc::new(Canned {
            // No retries: there is nothing to retry against.
            base: ProviderBase::new(0, 0.0, None),
            routes,
            next_route: AtomicUsize::new(0),
        })
    }
}

impl Provider for Canned {
    fn name(&self) -> &str {
        "canned"
    }

    fn base(&self) -> &ProviderBase {
        &self.base
    }

    /// Never reached: `chat_with_retries` below answers every call, and it is
    /// the one the session uses. A real backend writes this one instead.
    fn chat(
        &self,
        _model: &str,
        _messages: &[Value],
        _tools: Option<&[ToolSpec]>,
        _effort: ReasoningEffort,
    ) -> Result<ProviderResponse> {
        Ok(ProviderResponse::text("..."))
    }

    fn chat_with_retries(
        &self,
        _model: &str,
        _messages: &[Value],
        purpose: &str,
        _tools: Option<&[ToolSpec]>,
        _effort: ReasoningEffort,
    ) -> Result<ProviderResponse> {
        if purpose.contains("orchestrator") {
            let index = self.next_route.fetch_add(1, Ordering::SeqCst);
            let route = self.routes[index.min(self.routes.len() - 1)];
            return Ok(ProviderResponse::text(route));
        }
        if purpose.contains("final summary") {
            // The prose the caller reads, and the `result:` block the gameplan
            // declares. Both come out of the same closing turn.
            return Ok(ProviderResponse::text(
                "Both sides accepted write-through for the invalidation story.\n\n\
                 ```json\n\
                 {\"consensus\": true, \
                 \"summary\": \"Write-through, for the invalidation story.\"}\n\
                 ```",
            ));
        }
        Ok(ProviderResponse::text(
            "Write-through. The invalidation story is simpler and the write \
             cost is bounded.",
        ))
    }
}

fn main() -> Result<()> {
    let provider = Canned::new(vec![
        "@Alice, open the case.",
        "@Bob, answer that.",
        "CONSENSUS_REACHED",
    ]);

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Should the cache be write-through?".to_string(),
        provider: Some(provider),
        channel: Some(Arc::new(ConsoleChannel::default())),
        // The default second between turns is for a human watching a real
        // debate; there is nothing here to watch for.
        turn_delay: Duration::ZERO,
        ..Default::default()
    })?;

    session.add_participant(Agent::new("Alice", "canned-model"));
    session.add_participant(Agent::new("Bob", "canned-model"));
    session.add_orchestrator(Agent {
        role: Role::Orchestrator,
        ..Agent::new("Mod", "canned-model")
    })?;

    let result = session.run()?;

    println!("\n--- Result ---");
    println!("Turns:     {}", result.turns_completed);
    println!("Ended on:  {}", result.end_reason);
    println!("Consensus: {}", result.consensus_reached);
    println!("Summary:   {}", result.summary());
    println!("Fields:    {:?}", result.fields);
    Ok(())
}
