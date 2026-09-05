//! Choose one participant, inspect events, and finish with a validated result.
//! No orchestrator, API key, network, or framework background thread is needed.
//!
//! ```sh
//! cargo run -p kerness --example host_control
//! ```

mod support;

use std::sync::Arc;
use std::time::Duration;

use kerness::access::AccessPolicy;
use kerness::usage::RunBudget;
use kerness::{
    Agent, Error, RunEvent, RunInput, RunMode, RunOptions, RunReason, Session, SessionConfig,
    StepOutcome, WaitReason,
};
use serde_json::json;

use support::{Scripted, Workspace};

const GAMEPLAN: &str = "---\nname: host-control\nagents:\n  orchestrator: false\n  \
    participants: {min: 1, max: 1}\nloop:\n  max_turns: 4\n  max_rounds: 2\n  \
    terminate_on: [DONE]\nresult:\n  summary: {type: str}\n---\n\nAnswer the host's request.\n";

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new("host-control")?;
    let gameplan = workspace.write("gameplan.md", GAMEPLAN)?;
    let provider = Scripted::new(&["Write-through keeps the invalidation rules simple."]);
    let mut session = Session::new(SessionConfig {
        gameplan: gameplan.display().to_string(),
        topic: "Which cache policy should we use?".into(),
        provider: Some(provider.clone()),
        model: Some("offline-model".into()),
        memory: workspace.path().join("memory.md").display().to_string(),
        access_policy: Some(AccessPolicy {
            workspace: Some(workspace.path().display().to_string()),
            ..AccessPolicy::new()
        }),
        turn_delay: Duration::ZERO,
        ..SessionConfig::default()
    })?;
    session.add_agent(Agent::new("Advisor"))?;

    // start consumes configuration. The host owns this handle until completion.
    let mut run = session.start(RunOptions {
        mode: RunMode::HostDriven,
        budget: RunBudget {
            max_provider_operations: Some(1),
            ..RunBudget::default()
        },
        event_sink: Some(Arc::new(|event: &RunEvent| {
            println!("event {}: {:?}", event.sequence, event.event);
            Ok(())
        })),
        ..RunOptions::default()
    })?;
    let control = run.control(); // A UI may retain this and call cancel().
    let mut asked = false;
    loop {
        match run.step(RunInput::Continue)? {
            StepOutcome::Progress => {}
            StepOutcome::Waiting {
                reason: WaitReason::Input,
            } if !asked => {
                asked = true;
                run.step(RunInput::SelectAgent {
                    agent: "Advisor".into(),
                    instruction: "Recommend one cache policy.".into(),
                })?;
            }
            StepOutcome::Waiting {
                reason: WaitReason::Input,
            } => {
                // The host supplies the declared shape. Finish asks no model
                // for another answer or a judging pass.
                let outcome = run.step(RunInput::Finish {
                    result: json!({"summary": "Use write-through caching."}),
                })?;
                let StepOutcome::Finished { outcome } = outcome else {
                    return Err(Error::session("Finish did not complete the run").into());
                };
                assert_eq!(outcome.reason, RunReason::Completed);
                assert!(outcome.diagnostics.valid);
                assert_eq!(provider.calls(), 1);
                assert!(!control.is_cancelled());
                println!("Result: {}", outcome.result.fields["summary"]);
                println!("Usage: {}", serde_json::to_string(&outcome.usage)?);
                break;
            }
            other => return Err(Error::session(format!("Unexpected step: {other:?}")).into()),
        }
    }
    Ok(())
}
