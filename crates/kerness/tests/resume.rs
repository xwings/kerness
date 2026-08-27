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
use kerness::{Agent, Role, Session, SessionResult};
use serde_json::Value;

use common::{config, refusal, RecordingChannel, ScriptedProvider, TempDir};

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
    settings.max_turns = turns;
    settings.channel = channel;

    let mut session = Session::new(settings).expect("the gameplan loads");
    session.add_participant(Agent::new("P0", "gpt-4o"));
    session
        .add_orchestrator(Agent {
            role: Role::Orchestrator,
            ..Agent::new("Mod", "gpt-4o")
        })
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

    // A save follows the send that triggers it, so each delivery sees the
    // count from the turn before: nothing, then 1, then 2.
    assert_eq!(
        watcher.seen(),
        vec![None, Some(1), Some(2)],
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
    session.add_participant(Agent::new("P0", "gpt-4o"));
    session
        .add_orchestrator(Agent {
            role: Role::Orchestrator,
            ..Agent::new("Mod", "gpt-4o")
        })
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
    let mut session = Session::new(settings).expect("the gameplan loads");
    session.add_participant(Agent::new("P0", "gpt-4o"));
    session
        .add_orchestrator(Agent {
            role: Role::Orchestrator,
            ..Agent::new("Mod", "gpt-4o")
        })
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
    let mut session = Session::new(settings).expect("the gameplan loads");
    session.add_participant(Agent::new("Someone else", "gpt-4o"));
    session
        .add_orchestrator(Agent {
            role: Role::Orchestrator,
            ..Agent::new("Mod", "gpt-4o")
        })
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
    ];

    for (contents, expected) in cases {
        let temp = TempDir::new("resume");
        temp.write("run.json", contents);

        let path = temp.write("resumable.md", RESUMABLE);
        let mut settings = config(&path.to_string_lossy(), "Ship it?", provider());
        settings.session_file = Some(temp.str_join("run.json"));
        let mut session = Session::new(settings).expect("the gameplan loads");
        session.add_participant(Agent::new("P0", "gpt-4o"));
        session
            .add_orchestrator(Agent {
                role: Role::Orchestrator,
                ..Agent::new("Mod", "gpt-4o")
            })
            .expect("the roster has no orchestrator yet");

        let message = refusal(session.run());
        assert!(message.contains(expected), "{contents}: {message}");
    }
}
