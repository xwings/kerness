//! Session files: what a run writes, and what a second run makes of it.
//!
//! `sessionfile.rs` tests the reader and writer against snapshots it builds
//! itself. What is proved here is the feature: a session with a file behind it
//! saves after every turn, a later run of the same program picks up where that
//! one stopped, and a file that belongs to some other run is refused rather
//! than spliced in.

mod common;

use std::sync::{Arc, Mutex};

use kerness::channel::Channel;
use kerness::sessionfile::SCHEMA_VERSION;
use kerness::{Agent, Session, SessionResult};
use serde_json::Value;

use common::{config, confine, refusal, RecordingChannel, ScriptedProvider, TempDir};

/// Two turns' worth of budget, and rounds to spare — so the first run stops on
/// the turn ceiling with the phase list nowhere near finished.
const RESUMABLE: &str = r#"---
name: resumable
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_turns: 2
  max_rounds: 5
  terminate_on: [DONE]
---

# Resumable

Route to the participant each round.
"#;

fn provider() -> Arc<ScriptedProvider> {
    ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, go."])
        .on("final summary", &["Done."])
        .fallback(&["My position."])
        .shared()
}

/// Run [`RESUMABLE`] against *file*, with *turns* replacing the gameplan's
/// ceiling when given.
fn run(
    temp: &TempDir,
    file: &str,
    topic: &str,
    turns: Option<i64>,
    channel: Option<Arc<dyn Channel>>,
) -> SessionResult {
    let path = temp.write("resumable.md", RESUMABLE);
    let mut settings = config(&path.to_string_lossy(), topic, provider());
    settings.session_file = Some(temp.str_join(file));
    confine(&mut settings, temp);
    settings.max_turns = turns;
    settings.channel = channel;

    let mut session = Session::new(settings).expect("the gameplan loads");
    session
        .add_agent(Agent::new("P0").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session.run().expect("a scripted run cannot fail")
}

/// The saved file as JSON.
fn saved(temp: &TempDir, file: &str) -> Value {
    serde_json::from_str(&temp.read(file)).expect("the session file is JSON")
}

/// A channel that reads the session file every time a turn is delivered.
///
/// The only way to prove *when* a save happens is to look while the run is
/// still going, and a channel is the one seam a caller has inside the loop.
struct Watching {
    path: String,
    seen: Mutex<Vec<Option<i64>>>,
}

impl Watching {
    fn new(path: String) -> Arc<Watching> {
        Arc::new(Watching {
            path,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Option<i64>> {
        self.seen.lock().expect("seen lock").clone()
    }
}

impl Channel for Watching {
    fn send(&self, _sender: &str, _message: &str) -> kerness::Result<()> {
        let turn_count = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|payload| payload["loop"]["turn_count"].as_i64());
        self.seen.lock().expect("seen lock").push(turn_count);
        Ok(())
    }

    fn send_system(&self, _message: &str) -> kerness::Result<()> {
        Ok(())
    }

    fn type_name(&self) -> String {
        "Watching".to_string()
    }
}

/// The file is rewritten as each turn is recorded, not once at the end — which
/// is the whole point of a crash-resumable run.
#[test]
fn the_state_is_written_after_every_turn() {
    let temp = TempDir::new("resume");
    let watcher = Watching::new(temp.str_join("run.json"));

    run(&temp, "run.json", "Ship it?", None, Some(watcher.clone()));

    // The committed conversation and scheduler are durable before delivery.
    // The closing summary shares the last ordinary turn's count.
    assert_eq!(
        watcher.seen(),
        vec![Some(1), Some(2), Some(2)],
        "the file did not advance one turn at a time"
    );
}

/// What the file holds is the framework's own schema, tagged with the version
/// that wrote it, plus enough identity to recognise the run later.
#[test]
fn the_file_carries_its_version_and_the_run_it_belongs_to() {
    let temp = TempDir::new("resume");
    let result = run(&temp, "run.json", "Ship it?", None, None);
    let payload = saved(&temp, "run.json");

    assert_eq!(payload["version"], Value::from(SCHEMA_VERSION));
    assert_eq!(payload["identity"]["topic"], Value::from("Ship it?"));
    assert_eq!(payload["identity"]["orchestrator"], Value::from("Mod"));
    assert_eq!(payload["identity"]["participants"][0], Value::from("P0"));
    assert_eq!(
        payload["loop"]["turn_count"].as_i64(),
        Some(result.turns_completed)
    );
    assert!(
        !payload["transcript"]
            .as_array()
            .expect("a transcript")
            .is_empty(),
        "the transcript was not saved"
    );
}

/// The rename is what makes a crash mid-write survivable, and it leaves nothing
/// beside the file it replaced.
#[test]
fn no_temporary_file_is_left_behind() {
    let temp = TempDir::new("resume");
    run(&temp, "run.json", "Ship it?", None, None);

    assert_eq!(temp.entries(), vec!["resumable.md", "run.json"]);
}

/// A session that asked for no file writes none. Persistence is opt-in.
#[test]
fn a_session_without_a_file_persists_nothing() {
    let temp = TempDir::new("resume");
    let path = temp.write("resumable.md", RESUMABLE);
    let mut settings = config(&path.to_string_lossy(), "Ship it?", provider());
    settings.session_file = None;

    let mut session = Session::new(settings).expect("the gameplan loads");
    session
        .add_agent(Agent::new("P0").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session.run().expect("a scripted run cannot fail");

    assert_eq!(temp.entries(), vec!["resumable.md"]);
}

/// A missing file means "this is the first run": not an error, and not a reason
/// to announce a resume that did not happen.
#[test]
fn a_missing_file_is_the_first_run_and_says_nothing() {
    let temp = TempDir::new("resume");
    let channel = RecordingChannel::new();

    run(&temp, "run.json", "Ship it?", None, Some(channel.clone()));

    assert!(
        !channel.noted("Resumed from"),
        "a first run claimed to resume: {:?}",
        channel.system()
    );
    assert!(temp.exists("run.json"), "the run wrote no file");
}

/// The feature itself: the second run inherits the first one's turn count and
/// conversation, and says so.
#[test]
fn a_second_run_continues_where_the_first_stopped() {
    let temp = TempDir::new("resume");

    let first = run(&temp, "run.json", "Ship it?", None, None);
    assert_eq!(first.end_reason, "max_turns");
    assert_eq!(first.turns_completed, 2);

    let channel = RecordingChannel::new();
    let second = run(
        &temp,
        "run.json",
        "Ship it?",
        Some(6),
        Some(channel.clone()),
    );

    assert!(
        channel.noted("Resumed from"),
        "the resume was silent: {:?}",
        channel.system()
    );
    assert_eq!(second.end_reason, "max_turns");
    assert_eq!(
        second.turns_completed, 6,
        "the turn budget restarted instead of continuing"
    );
    assert!(
        second.history.len() > first.history.len(),
        "the second run did not build on the first"
    );

    // A version-1 turn-boundary snapshot has no suspended runtime/scheduler.
    // It migrates by retaining the transcript and already spent turn count.
    let legacy_temp = TempDir::new("resume-v1");
    let legacy_first = run(&legacy_temp, "run.json", "Ship it?", None, None);
    let mut legacy = saved(&legacy_temp, "run.json");
    legacy["version"] = Value::from(1);
    legacy["loop"].as_object_mut().unwrap().remove("runtime");
    legacy["loop"].as_object_mut().unwrap().remove("scheduler");
    std::fs::write(
        legacy_temp.join("run.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let migrated = run(&legacy_temp, "run.json", "Ship it?", Some(6), None);
    assert_eq!(migrated.turns_completed, 6);
    assert!(migrated.history.starts_with(&legacy_first.history));
    assert_eq!(
        saved(&legacy_temp, "run.json")["version"],
        Value::from(SCHEMA_VERSION)
    );
}

/// Resume is automatic, so a stale file at the same path would otherwise splice
/// an unrelated conversation in with nothing said. The refusal names the first
/// field that differs and both of its values.
#[test]
fn a_file_written_for_a_different_run_is_refused_by_name() {
    let temp = TempDir::new("resume");
    run(&temp, "run.json", "Ship it?", None, None);

    let path = temp.write("resumable.md", RESUMABLE);
    let mut settings = config(&path.to_string_lossy(), "Ship something else?", provider());
    settings.session_file = Some(temp.str_join("run.json"));
    confine(&mut settings, &temp);
    let mut session = Session::new(settings).expect("the gameplan loads");
    session
        .add_agent(Agent::new("P0").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let message = refusal(session.run());
    assert!(message.contains("topic was 'Ship it?'"), "{message}");
    assert!(message.contains("'Ship something else?'"), "{message}");
    assert!(message.contains("Delete the file"), "{message}");
}

/// A roster change is the other half of identity: same topic, different cast.
#[test]
fn a_file_written_for_a_different_roster_is_refused() {
    let temp = TempDir::new("resume");
    run(&temp, "run.json", "Ship it?", None, None);

    let path = temp.write("resumable.md", RESUMABLE);
    let mut settings = config(&path.to_string_lossy(), "Ship it?", provider());
    settings.session_file = Some(temp.str_join("run.json"));
    confine(&mut settings, &temp);
    let mut session = Session::new(settings).expect("the gameplan loads");
    session
        .add_agent(Agent::new("Someone else").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let message = refusal(session.run());
    assert!(message.contains("participants was"), "{message}");
}

/// A file that is not a snapshot fails loudly. Each shape says what is wrong
/// with it and offers the same two ways out.
#[test]
fn a_file_that_is_not_a_snapshot_is_refused_with_a_reason() {
    let cases = [
        ("not json at all", "not valid JSON"),
        ("[1, 2, 3]", "does not hold a JSON object"),
        (r#"{"version": 99}"#, "schema version 99"),
        (r#"{"version": 1}"#, "invalid or missing snapshot envelope"),
        (r#"{"version": 2}"#, "invalid or missing snapshot envelope"),
    ];

    for (contents, expected) in cases {
        let temp = TempDir::new("resume");
        temp.write("run.json", contents);

        let path = temp.write("resumable.md", RESUMABLE);
        let mut settings = config(&path.to_string_lossy(), "Ship it?", provider());
        settings.session_file = Some(temp.str_join("run.json"));
        confine(&mut settings, &temp);
        let mut session = Session::new(settings).expect("the gameplan loads");
        session
            .add_agent(Agent::new("P0").with_model("gpt-4o"))
            .expect("add agent");
        session
            .add_agent(
                Agent::new("Mod")
                    .with_model("gpt-4o")
                    .with_role("orchestrator"),
            )
            .expect("the roster has no orchestrator yet");

        let message = refusal(session.run());
        assert!(message.contains(expected), "{contents}: {message}");
    }
}

const HOST_RESUME: &str = r#"---
name: host_resume
agents:
  orchestrator: false
  participants: {min: 1}
loop:
  max_turns: 10
  max_rounds: 5
---
# Resumable host
"#;

fn host_session(
    temp: &TempDir,
    provider: Arc<common::ToolProvider>,
    handler: Arc<dyn kerness::ContextToolHandler>,
) -> Session {
    let path = temp.write("host_resume.md", HOST_RESUME);
    let mut settings = config(&path.to_string_lossy(), "Resume tools", provider);
    confine(&mut settings, temp);
    settings.session_file = Some(temp.str_join("run.json"));
    let mut session = Session::new(settings).unwrap();
    session
        .add_agent(Agent::new("P0").with_model("model"))
        .unwrap();
    session
        .add_contextual_tool(kerness::ContextToolSpec::new(
            "record",
            "Record progress",
            serde_json::json!({"type": "object"}),
            handler,
        ))
        .unwrap();
    session
}

fn host_options() -> kerness::RunOptions {
    kerness::RunOptions {
        mode: kerness::RunMode::HostDriven,
        ..kerness::RunOptions::default()
    }
}

fn waiting(run: &mut kerness::SessionRun) -> kerness::WaitReason {
    for _ in 0..30 {
        match run.step(kerness::RunInput::Continue).unwrap() {
            kerness::StepOutcome::Progress => {}
            kerness::StepOutcome::Waiting { reason } => return reason,
            kerness::StepOutcome::Finished { outcome } => {
                panic!("unexpected terminal outcome: {outcome:?}")
            }
        }
    }
    panic!("run did not reach a waiting boundary")
}

#[test]
fn a_saved_native_approval_resumes_after_completed_tools_without_replaying_them() {
    use kerness::tooling::Arguments;
    use kerness::{
        ContextToolHandler, PreflightAction, RunInput, ToolContext, ToolIdentity, WaitReason,
    };
    use serde_json::json;

    struct ConfirmSecond(Arc<Mutex<Vec<(String, String)>>>);
    impl ContextToolHandler for ConfirmSecond {
        fn preflight(
            &self,
            arguments: &Arguments,
            _: &ToolIdentity,
        ) -> kerness::Result<Option<PreflightAction>> {
            Ok(
                (arguments["value"] == "second").then(|| PreflightAction::Confirm {
                    description: "Record second".into(),
                }),
            )
        }
        fn call(&self, arguments: &Arguments, context: &ToolContext) -> kerness::Result<String> {
            let value = arguments["value"].as_str().unwrap().to_string();
            self.0
                .lock()
                .unwrap()
                .push((context.identity().call_id().into(), value.clone()));
            Ok(value)
        }
    }

    let temp = TempDir::new("approval-resume");
    let first = common::tool_call_reply("record", json!({"value": "first"}), "c1");
    let second = common::tool_call_reply("record", json!({"value": "second"}), "c2");
    let provider = common::ToolProvider::new(
        kerness::ToolDialect::Openai,
        vec![
            kerness::ProviderResponse::text("Earlier committed turn."),
            kerness::ProviderResponse {
                tool_calls: [first.tool_calls, second.tool_calls].concat(),
                ..kerness::ProviderResponse::default()
            },
            kerness::ProviderResponse::text("Both recorded."),
        ],
    )
    .shared();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(ConfirmSecond(calls.clone()));
    let mut first_run = host_session(&temp, provider.clone(), handler.clone())
        .start(host_options())
        .unwrap();
    first_run
        .step(RunInput::SelectAgent {
            agent: "P0".into(),
            instruction: "Commit an earlier turn".into(),
        })
        .unwrap();
    assert_eq!(waiting(&mut first_run), WaitReason::Input);
    assert_eq!(saved(&temp, "run.json")["loop"]["turn_count"], json!(1));
    first_run
        .step(RunInput::SelectAgent {
            agent: "P0".into(),
            instruction: "Record both".into(),
        })
        .unwrap();
    let WaitReason::Approval { request } = waiting(&mut first_run) else {
        panic!("second call needs approval")
    };
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(provider.call_count(), 2);
    let first_id = calls.lock().unwrap()[0].0.clone();
    first_run.checkpoint().unwrap();
    assert_eq!(
        saved(&temp, "run.json")["loop"]["runtime"]["approval"]["request_id"],
        request.request_id
    );
    let suspended = saved(&temp, "run.json");
    drop(first_run);

    for field in [
        "turn_id",
        "next_call",
        "next_event",
        "request_id",
        "call_id",
    ] {
        let mut malformed = suspended.clone();
        let runtime = &mut malformed["loop"]["runtime"];
        let expected = if field == "request_id" {
            runtime["approval"][field] = json!("mismatched-request");
            "approval identity"
        } else if field == "call_id" {
            runtime["approval"]["request_id"] = json!("forged-call");
            runtime["approval"]["identity"][field] = json!("forged-call");
            "action does not match its active turn"
        } else {
            runtime[field] = json!(u64::MAX);
            "exhausted identity counters"
        };
        std::fs::write(
            temp.join("run.json"),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();
        let error =
            refusal(host_session(&temp, provider.clone(), handler.clone()).start(host_options()));
        assert!(error.contains(expected), "{field}: {error}");
        assert_eq!(provider.call_count(), 2, "malformed state cannot dispatch");
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
    std::fs::write(
        temp.join("run.json"),
        serde_json::to_vec(&suspended).unwrap(),
    )
    .unwrap();

    let mut resumed = host_session(&temp, provider.clone(), handler.clone())
        .start(host_options())
        .unwrap();
    // A host may checkpoint immediately after restoring. The scheduler must
    // already own its prior progress before any next_action/provider call.
    resumed.checkpoint().unwrap();
    let resaved = saved(&temp, "run.json");
    assert_eq!(resaved["loop"]["turn_count"], json!(1));
    assert_eq!(resaved["loop"]["phases"], suspended["loop"]["phases"]);
    for field in ["active", "approval", "turn_id", "next_call"] {
        assert_eq!(
            resaved["loop"]["runtime"][field], suspended["loop"]["runtime"][field],
            "saved {field} changed without execution"
        );
    }
    assert_eq!(
        waiting(&mut resumed),
        WaitReason::Approval {
            request: request.clone()
        }
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(provider.call_count(), 2);
    drop(resumed);

    let mut cancelled = host_session(&temp, provider.clone(), handler.clone())
        .start(host_options())
        .unwrap();
    cancelled.control().cancel();
    let kerness::StepOutcome::Finished { outcome } = cancelled.step(RunInput::Continue).unwrap()
    else {
        panic!("immediate cancellation terminates the restored run")
    };
    assert_eq!(outcome.reason, kerness::RunReason::Cancelled);
    assert_eq!(outcome.result.turns_completed, 1);
    assert!(outcome
        .result
        .history
        .iter()
        .any(|message| message.content == "Earlier committed turn."));
    assert_eq!(provider.call_count(), 2);
    assert_eq!(calls.lock().unwrap().len(), 1);
    drop(cancelled);

    std::fs::write(temp.join("run.json"), serde_json::to_vec(&resaved).unwrap()).unwrap();
    let mut resumed = host_session(&temp, provider.clone(), handler)
        .start(host_options())
        .unwrap();
    assert_eq!(
        waiting(&mut resumed),
        WaitReason::Approval {
            request: request.clone()
        }
    );
    resumed
        .step(RunInput::Approve {
            request_id: request.request_id,
            approved: true,
        })
        .unwrap();
    assert_eq!(waiting(&mut resumed), WaitReason::Input);
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0], (first_id, "first".into()));
    assert_eq!(
        recorded[1],
        (request.identity.call_id().to_string(), "second".into())
    );
    assert_eq!(resumed.usage().tool_calls, 2);
    assert_eq!(provider.call_count(), 3);
    assert_eq!(saved(&temp, "run.json")["loop"]["turn_count"], json!(2));
    let followup = provider.calls()[2].clone();
    let results: Vec<_> = followup
        .messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| (message["tool_call_id"].clone(), message["content"].clone()))
        .collect();
    assert_eq!(
        results,
        vec![
            (json!("c1"), json!("first")),
            (json!("c2"), json!("second"))
        ]
    );
}

#[test]
fn an_intent_without_completion_requires_reconciliation_and_never_replays_the_handler() {
    use kerness::tooling::Arguments;
    use kerness::{
        ContextToolHandler, RunInput, RunReason, StepOutcome, ToolContext, ToolResult, WaitReason,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CaptureIntent {
        current: PathBuf,
        intent: PathBuf,
        calls: Arc<AtomicUsize>,
        fail_completion: bool,
    }
    impl ContextToolHandler for CaptureIntent {
        fn call(&self, _: &Arguments, _: &ToolContext) -> kerness::Result<String> {
            // The real handler observes the on-disk intent published before
            // its side effect, capturing exactly a crash's recoverable state.
            std::fs::copy(&self.current, &self.intent).unwrap();
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_completion {
                std::fs::remove_file(&self.current).unwrap();
                std::fs::create_dir(&self.current).unwrap();
            }
            Ok("recorded".into())
        }
    }

    for fail_completion in [false, true] {
        let temp = TempDir::new("indeterminate");
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = common::ToolProvider::new(
            kerness::ToolDialect::Openai,
            vec![
                common::tool_call_reply("record", json!({}), "c1"),
                kerness::ProviderResponse::text("Recorded."),
            ],
        )
        .shared();
        let handler = Arc::new(CaptureIntent {
            current: temp.join("run.json"),
            intent: temp.join("intent.json"),
            calls: calls.clone(),
            fail_completion,
        });
        let mut first = host_session(&temp, provider.clone(), handler.clone())
            .start(host_options())
            .unwrap();
        first
            .step(RunInput::SelectAgent {
                agent: "P0".into(),
                instruction: "Record once".into(),
            })
            .unwrap();
        for _ in 0..20 {
            let outcome = first.step(RunInput::Continue);
            if calls.load(Ordering::SeqCst) == 1 {
                if fail_completion {
                    assert!(outcome.is_err(), "completion save must report failure");
                } else {
                    outcome.unwrap();
                }
                break;
            }
            outcome.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.call_count(), 1);
        let captured = saved(&temp, "intent.json");
        assert!(captured["loop"]["runtime"]["intent"].is_object());
        assert_eq!(captured["loop"]["runtime"]["usage"]["tool_calls"], json!(1));
        drop(first);
        if fail_completion {
            std::fs::remove_dir(temp.join("run.json")).unwrap();
        }
        std::fs::copy(temp.join("intent.json"), temp.join("run.json")).unwrap();

        let mut resumed = host_session(&temp, provider.clone(), handler.clone())
            .start(host_options())
            .unwrap();
        let WaitReason::Indeterminate {
            action_id,
            call,
            identity,
        } = waiting(&mut resumed)
        else {
            panic!("intent must not replay")
        };
        assert_eq!(identity.call_id(), action_id);
        assert_eq!(call.id, "c1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.call_count(), 1);
        let result = ToolResult {
            name: "record".into(),
            content: "recorded".into(),
            is_error: false,
        };
        assert!(resumed
            .step(RunInput::Reconcile {
                action_id: "stale".into(),
                result: result.clone()
            })
            .is_err());
        assert!(resumed
            .step(RunInput::Reconcile {
                action_id: action_id.clone(),
                result: ToolResult {
                    name: "other".into(),
                    ..result.clone()
                }
            })
            .is_err());
        resumed
            .step(RunInput::Reconcile { action_id, result })
            .unwrap();
        assert_eq!(waiting(&mut resumed), WaitReason::Input);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "reconciliation supplies the result without replay"
        );
        assert_eq!(resumed.usage().tool_calls, 1);
        assert_eq!(provider.call_count(), 2);
        assert!(provider.calls()[1]
            .messages
            .iter()
            .any(|message| message["tool_call_id"] == "c1" && message["content"] == "recorded"));
        drop(resumed);

        // Cancelling the same captured intent is also safe and costs no call.
        std::fs::copy(temp.join("intent.json"), temp.join("run.json")).unwrap();
        let mut aborted = host_session(&temp, provider.clone(), handler)
            .start(host_options())
            .unwrap();
        aborted.control().cancel();
        let StepOutcome::Finished { outcome } = aborted.step(RunInput::Continue).unwrap() else {
            panic!("cancelled intent terminates")
        };
        assert_eq!(outcome.reason, RunReason::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.call_count(), 2);
    }
}

#[test]
fn a_failed_intent_write_prevents_the_tool_side_effect() {
    use kerness::tooling::Arguments;
    use kerness::{RunEventKind, RunInput, ToolContext};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let temp = TempDir::new("intent-write-failure");
    let provider = common::ToolProvider::new(
        kerness::ToolDialect::Openai,
        vec![common::tool_call_reply("record", json!({}), "c1")],
    )
    .shared();
    let calls = Arc::new(AtomicUsize::new(0));
    let observing = calls.clone();
    let handler = Arc::new(move |_: &Arguments, _: &ToolContext| {
        observing.fetch_add(1, Ordering::SeqCst);
        Ok("recorded".into())
    });
    let mut run = host_session(&temp, provider.clone(), handler)
        .start(host_options())
        .unwrap();
    run.step(RunInput::SelectAgent {
        agent: "P0".into(),
        instruction: "Record".into(),
    })
    .unwrap();
    for _ in 0..20 {
        run.step(RunInput::Continue).unwrap();
        if run
            .drain_events()
            .iter()
            .any(|event| matches!(event.event, RunEventKind::ProviderFinished { .. }))
        {
            break;
        }
    }
    assert_eq!(provider.call_count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    std::fs::remove_file(temp.join("run.json")).unwrap();
    std::fs::create_dir(temp.join("run.json")).unwrap();
    assert!(run.step(RunInput::Continue).is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "effect requires a durable intent"
    );
    assert_eq!(provider.call_count(), 1);
}
