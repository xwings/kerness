//! Save an approval, drop the run, then resume without repeating a finished tool.
//!
//! ```sh
//! cargo run -p kerness --example resume_approval
//! ```

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kerness::access::AccessPolicy;
use kerness::tooling::Arguments;
use kerness::{
    Agent, ApprovalRequest, ContextToolHandler, ContextToolSpec, Error, PreflightAction, Result,
    RunInput, RunMode, RunOptions, RunReason, Session, SessionConfig, SessionRun, StepOutcome,
    ToolContext, ToolIdentity, WaitReason,
};
use serde_json::json;

use support::{Scripted, Workspace};

const GAMEPLAN: &str = "---\nname: resume-approval\nagents:\n  orchestrator: false\n  \
    participants: {min: 1, max: 1}\nloop:\n  max_turns: 4\n  max_rounds: 2\n  \
    terminate_on: [DONE]\ntools: [record_note]\nresult:\n  saved: {type: int}\n---\n\nSave the requested notes.\n";

const CALLS: &str = r#"```tool_calls
[
  {"id":"first","name":"record_note","arguments":{"note":"Before the pause."}},
  {"id":"second","name":"record_note","arguments":{"note":"After the resume."}}
]
```"#;

struct RecordNote(Arc<AtomicUsize>);

impl ContextToolHandler for RecordNote {
    fn preflight(
        &self,
        arguments: &Arguments,
        identity: &ToolIdentity,
    ) -> Result<Option<PreflightAction>> {
        // Preflight describes the action without appending or incrementing.
        Ok(Some(PreflightAction::Confirm {
            description: format!("{}: save {}", identity.actor(), arguments["note"]),
        }))
    }

    fn call(&self, arguments: &Arguments, context: &ToolContext) -> Result<String> {
        let note = arguments["note"]
            .as_str()
            .ok_or_else(|| Error::session("Expected a note"))?;
        context.write_memory(note)?;
        self.0.fetch_add(1, Ordering::SeqCst);
        println!(
            "{} saved a note ({})",
            context.identity().actor(),
            context.identity().call_id()
        );
        Ok("Saved.".into())
    }
}

fn prepare(
    workspace: &Workspace,
    provider: Arc<Scripted>,
    calls: Arc<AtomicUsize>,
) -> Result<SessionRun> {
    let mut session = Session::new(SessionConfig {
        gameplan: workspace.path().join("gameplan.md").display().to_string(),
        topic: "Save two notes with host approval.".into(),
        provider: Some(provider),
        model: Some("offline-model".into()),
        memory: workspace.path().join("memory.md").display().to_string(),
        memory_write: true,
        session_file: Some(workspace.path().join("run.json").display().to_string()),
        access_policy: Some(AccessPolicy {
            workspace: Some(workspace.path().display().to_string()),
            ..AccessPolicy::new()
        }),
        turn_delay: Duration::ZERO,
        ..SessionConfig::default()
    })?;
    session.add_agent(Agent::new("Archivist"))?;
    session.add_contextual_tool(ContextToolSpec::new(
        "record_note",
        "Save a note after host confirmation.",
        json!({"type": "object", "properties": {"note": {"type": "string"}}, "required": ["note"]}),
        Arc::new(RecordNote(calls)),
    ))?;
    session.start(RunOptions {
        mode: RunMode::HostDriven,
        // Keep this stable when rebinding the same callback implementation.
        binding_version: "resume-approval-example-v1".into(),
        ..RunOptions::default()
    })
}

fn approval(run: &mut SessionRun) -> Result<ApprovalRequest> {
    loop {
        match run.step(RunInput::Continue)? {
            StepOutcome::Progress => {}
            StepOutcome::Waiting {
                reason: WaitReason::Approval { request },
            } => return Ok(request),
            other => {
                return Err(Error::session(format!(
                    "Expected an approval, got {other:?}"
                )))
            }
        }
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::new("resume-approval")?;
    workspace.write("gameplan.md", GAMEPLAN)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let first_provider = Scripted::new(&[CALLS]);
    let mut run = prepare(&workspace, first_provider.clone(), calls.clone())?;
    run.step(RunInput::SelectAgent {
        agent: "Archivist".into(),
        instruction: "Save both notes.".into(),
    })?;

    let first = approval(&mut run)?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    run.step(RunInput::Approve {
        request_id: first.request_id,
        approved: true,
    })?;
    let pending = approval(&mut run)?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first_provider.calls(), 1);
    run.checkpoint()?;
    println!("Saved pending approval {}", pending.request_id);
    drop(run);

    // A real host reattaches the same provider and callback implementations.
    // This script supplies only the response still outstanding after restore.
    let resumed_provider = Scripted::new(&["Both notes are saved."]);
    let mut resumed = prepare(&workspace, resumed_provider.clone(), calls.clone())?;
    let restored = approval(&mut resumed)?;
    assert_eq!(restored, pending);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    resumed.step(RunInput::Approve {
        request_id: restored.request_id,
        approved: true,
    })?;
    loop {
        match resumed.step(RunInput::Continue)? {
            StepOutcome::Progress => {}
            StepOutcome::Waiting {
                reason: WaitReason::Input,
            } => break,
            other => {
                return Err(Error::session(format!("Unexpected resumed step: {other:?}")).into())
            }
        }
    }
    let StepOutcome::Finished { outcome } = resumed.step(RunInput::Finish {
        result: json!({"saved": 2}),
    })?
    else {
        return Err(Error::session("Finish did not complete the run").into());
    };
    assert_eq!(outcome.reason, RunReason::Completed);
    assert!(outcome.diagnostics.valid);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(resumed_provider.calls(), 1);
    let memory = std::fs::read_to_string(workspace.path().join("memory.md"))?;
    assert_eq!(memory.matches("Before the pause.").count(), 1);
    assert_eq!(memory.matches("After the resume.").count(), 1);
    println!("Two approved notes, each saved once. Workspace cleaned on exit.");
    Ok(())
}
