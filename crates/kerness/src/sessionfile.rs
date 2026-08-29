//! The on-disk state of one run, and nothing else.
//!
//! `SessionResult::history` is a report, not something a second run can start
//! from. This module is what an interrupted session resumes from instead of
//! losing every provider call it has already paid for.
//!
//! This is **not** memory. `memory.md` is durable, user-owned prose that
//! outlives any run and has no schema (see [`crate::memory`]). A session file
//! is machine state for exactly one run — the conversation, the loop counters,
//! and where in the gameplan's phases it stopped — written in a schema this
//! module owns.
//!
//! The file is a single JSON object rewritten whole. Appending would be cheaper
//! per turn, but compaction rewrites history rather than extending it, so an
//! append-only log would need a rewrite path anyway; and the `.jsonl` shape is
//! already taken by [`crate::channel::LogChannel`], which is transcript output
//! rather than resumable state.

use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::conversation::{Message, Turn};
use crate::error::{Error, Result};
use crate::pyfmt;

/// Bumped whenever the shape below changes incompatibly.
///
/// A file from a version this build does not know is rejected rather than
/// guessed at: the fields it is missing would otherwise resume as silent
/// defaults.
pub const SCHEMA_VERSION: i64 = 1;

/// Identity fields, in the order a mismatch is reported.
///
/// Checked because resume is automatic — a stale file left at a path would
/// otherwise splice a previous run's conversation into an unrelated one with
/// nothing said.
const IDENTITY_FIELDS: [&str; 4] = ["gameplan", "topic", "participants", "orchestrator"];

/// Everything needed to continue a run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub identity: Map<String, Value>,
    pub turns: Vec<Turn>,
    pub transcript: Vec<Message>,
    /// Loop counters and phase position; see `OrchestratorLoop::snapshot`.
    /// Stored under the `loop` key, which is a keyword here and nowhere else.
    pub loop_state: Map<String, Value>,
    /// How many times this session has compacted, for the record.
    pub compactions: i64,
}

impl SessionSnapshot {
    /// A snapshot of a run that has said nothing yet.
    pub fn new(identity: Map<String, Value>) -> Self {
        SessionSnapshot {
            identity,
            ..SessionSnapshot::default()
        }
    }
}

/// Describe the run a snapshot belongs to.
///
/// Participants are sorted because registration order is not part of what makes
/// two runs the same session, and refusing to resume over a reordered
/// `add_agent` sequence would be a false alarm.
pub fn identity_for(
    gameplan: &str,
    topic: &str,
    participants: &[String],
    orchestrator: &str,
) -> Map<String, Value> {
    let mut sorted = participants.to_vec();
    sorted.sort();
    let mut identity = Map::new();
    identity.insert("gameplan".to_string(), json!(gameplan));
    identity.insert("topic".to_string(), json!(topic));
    identity.insert("participants".to_string(), json!(sorted));
    identity.insert("orchestrator".to_string(), json!(orchestrator));
    identity
}

/// Fail unless *saved* describes the same run as *current*.
///
/// The error names the first field that differs, both values, and the two ways
/// out. A caller who hits this has a stale file, and "cannot resume" alone
/// would not tell them which one.
pub fn check_identity(saved: &Map<String, Value>, current: &Map<String, Value>) -> Result<()> {
    for name in IDENTITY_FIELDS {
        let was = saved.get(name).unwrap_or(&Value::Null);
        let now = current.get(name).unwrap_or(&Value::Null);
        if was == now {
            continue;
        }
        return Err(Error::Session(format!(
            "Session file was written for a different run: {name} was {}, this \
             session has {}. Delete the file to start fresh, or pass a \
             different session_file path.",
            pyfmt::repr(was),
            pyfmt::repr(now)
        )));
    }
    Ok(())
}

/// Write *snapshot* to *path*, replacing whatever is there.
///
/// Written to a sibling temp file and renamed. A crash partway through a direct
/// write would leave a truncated file exactly where a resumable one belongs,
/// which is the one moment this feature exists to survive.
pub fn save_snapshot(path: &Path, snapshot: &SessionSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::Io(format!("{}: {err}", parent.display())))?;
        }
    }

    let payload = json!({
        "version": SCHEMA_VERSION,
        "identity": snapshot.identity,
        "turns": snapshot.turns.iter().map(turn_to_value).collect::<Vec<_>>(),
        "transcript": snapshot.transcript.iter().map(message_to_value).collect::<Vec<_>>(),
        "loop": snapshot.loop_state,
        "compactions": snapshot.compactions,
    });

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, pyfmt::json_dumps_indent2(&payload) + "\n")
        .map_err(|err| Error::Io(format!("{}: {err}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(|err| Error::Io(format!("{}: {err}", path.display())))
}

/// Read *path*, or return `None` when there is nothing to resume.
///
/// A missing file is not an error and is not created — the same rule
/// [`crate::memory::Memory::load`] follows. It simply means this is the first
/// run. A file that exists but is not readable as a snapshot fails.
pub fn load_snapshot(path: &Path) -> Result<Option<SessionSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).map_err(|err| Error::Io(format!("{}: {err}", path.display())))?;
    let payload: Value = serde_json::from_str(&text).map_err(|err| {
        Error::Session(format!(
            "Session file {} is not valid JSON: {err}. Delete it to start \
             fresh, or pass a different session_file path.",
            path.display()
        ))
    })?;
    let Value::Object(payload) = payload else {
        return Err(Error::Session(format!(
            "Session file {} does not hold a JSON object.",
            path.display()
        )));
    };

    let version = payload.get("version").unwrap_or(&Value::Null);
    if version.as_i64() != Some(SCHEMA_VERSION) {
        return Err(Error::Session(format!(
            "Session file {} has schema version {}; this build of kerness \
             writes and reads version {SCHEMA_VERSION}. Delete it to start \
             fresh, or pass a different session_file path.",
            path.display(),
            pyfmt::repr(version)
        )));
    }

    Ok(Some(SessionSnapshot {
        identity: object_at(&payload, "identity"),
        turns: array_at(&payload, "turns")
            .iter()
            .map(turn_from_value)
            .collect(),
        transcript: array_at(&payload, "transcript")
            .iter()
            .map(message_from_value)
            .collect(),
        loop_state: object_at(&payload, "loop"),
        compactions: payload
            .get("compactions")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    }))
}

fn object_at(payload: &Map<String, Value>, key: &str) -> Map<String, Value> {
    payload
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn array_at<'a>(payload: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The text at *key*, or *fallback* when the field is missing or not a string.
fn text_at(data: &Value, key: &str, fallback: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn turn_to_value(turn: &Turn) -> Value {
    json!({
        "role": turn.role,
        "speaker": turn.speaker,
        "content": turn.content,
        "round_idx": turn.round_idx,
        "msg_type": turn.msg_type,
    })
}

fn turn_from_value(data: &Value) -> Turn {
    Turn {
        role: text_at(data, "role", "user"),
        speaker: text_at(data, "speaker", ""),
        content: text_at(data, "content", ""),
        round_idx: data.get("round_idx").and_then(Value::as_i64).unwrap_or(0),
        msg_type: text_at(data, "msg_type", "turn"),
    }
}

fn message_to_value(message: &Message) -> Value {
    json!({
        "sender": message.sender,
        "content": message.content,
        "round_idx": message.round_idx,
        "msg_type": message.msg_type,
    })
}

fn message_from_value(data: &Value) -> Message {
    Message {
        sender: text_at(data, "sender", ""),
        content: text_at(data, "content", ""),
        round_idx: data.get("round_idx").and_then(Value::as_i64).unwrap_or(0),
        msg_type: text_at(data, "msg_type", "turn"),
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::conversation::Conversation;
    use crate::testing::TempDir;

    /// A directory that removes itself, so these tests leave no trace either.
    fn identity() -> Map<String, Value> {
        identity_for(
            "debate",
            "Test",
            &["Alice".to_string(), "Bob".to_string()],
            "Mod",
        )
    }

    #[test]
    fn every_kind_of_record_survives() {
        // Conversation keeps directives, agent turns, and system notes in
        // different places — turns only, both, and transcript only — and
        // render() folds speaker and msg_type into a string. A snapshot that
        // round-trips one kind proves nothing about the others, and one that
        // persists the rendered form resumes a session that cannot tell an
        // agent turn from a directive. The loop state is what stops a resume
        // from replaying a whole phase.
        let mut conversation = Conversation::default();
        conversation.directive("The topic");
        conversation.say("Alice", "My position", 1, "turn");
        conversation.note("something happened");
        conversation.say("Mod", "done", 4, "final_summary");

        let mut turns = conversation.turns().to_vec();
        turns.push(Turn {
            role: "assistant".to_string(),
            speaker: "Alice".to_string(),
            content: "hi".to_string(),
            round_idx: 7,
            msg_type: "final_summary".to_string(),
        });
        let loop_state = json!({"turn_count": 5, "phases": {"index": 1, "pending": ["Bob"]}})
            .as_object()
            .cloned()
            .expect("object");

        let dir = TempDir::new("round-trip");
        let path = dir.join("run.json");
        save_snapshot(
            &path,
            &SessionSnapshot {
                identity: identity(),
                turns: turns.clone(),
                transcript: conversation.transcript().to_vec(),
                loop_state: loop_state.clone(),
                compactions: 3,
            },
        )
        .expect("save");
        let loaded = load_snapshot(&path).expect("load").expect("a saved run");

        assert_eq!(loaded.turns, turns);
        assert_eq!(loaded.transcript, conversation.transcript());
        assert_eq!(loaded.loop_state, loop_state);
        assert_eq!(loaded.compactions, 3);
    }

    #[test]
    fn it_writes_readable_json() {
        // The file is meant to be inspectable when a resume goes wrong.
        let dir = TempDir::new("readable");
        let path = dir.join("run.json");
        save_snapshot(&path, &SessionSnapshot::new(identity())).expect("save");

        let payload: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("valid JSON");
        assert_eq!(payload["version"], json!(SCHEMA_VERSION));
        assert_eq!(payload["identity"]["topic"], json!("Test"));
    }

    #[test]
    fn a_missing_file_is_not_an_error_and_is_not_created() {
        // It just means this is the first run. Creating it here would leave a
        // trace on disk for a session that had nothing to say yet.
        let dir = TempDir::new("absent");
        let path = dir.join("absent.json");

        assert!(load_snapshot(&path)
            .expect("no file is not a failure")
            .is_none());
        assert!(!path.exists());
    }

    #[test]
    fn an_unknown_schema_version_is_refused_naming_both() {
        // Missing fields would otherwise resume as silent defaults — a phase
        // pointer of zero reads as "start over", not as "unknown" — and a
        // caller cannot act on the refusal without knowing what this build
        // speaks.
        let dir = TempDir::new("version");
        let path = dir.join("run.json");
        fs::write(&path, r#"{"version": 999}"#).expect("write");

        let error = load_snapshot(&path).expect_err("a version this build cannot read");
        assert!(matches!(error, Error::Session(_)), "{error:?}");
        assert!(error.to_string().contains("999"), "{error}");
        assert!(
            error.to_string().contains(&SCHEMA_VERSION.to_string()),
            "{error}"
        );
    }

    #[test]
    fn unparseable_json_names_the_file() {
        // A truncated file should say which one, not fail from somewhere
        // inside a run.
        let dir = TempDir::new("truncated");
        let path = dir.join("run.json");
        fs::write(&path, "{not json").expect("write");

        let error = load_snapshot(&path).expect_err("half a file is not a snapshot");
        assert!(error.to_string().contains("run.json"), "{error}");
    }

    #[test]
    fn a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else() {
        // The rename is what makes a save atomic; a leftover .tmp would mean it
        // did not happen. Missing parents are the caller's layout, not an error
        // to raise mid-run.
        let dir = TempDir::new("atomic");
        let nested = dir.join("nested").join("deeper").join("run.json");
        save_snapshot(&nested, &SessionSnapshot::new(identity())).expect("save");
        assert!(nested.exists());

        save_snapshot(&dir.join("run.json"), &SessionSnapshot::new(identity())).expect("save");
        let mut left: Vec<String> = fs::read_dir(&dir.0)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(left, ["nested", "run.json"]);
    }

    #[test]
    fn the_same_run_passes_however_it_was_registered() {
        // Participants are sorted because add_agent order is not what
        // makes two runs the same session. Refusing over it would be a false
        // alarm on a script whose calls were merely reordered.
        check_identity(&identity(), &identity()).expect("the same run");
        check_identity(
            &identity(),
            &identity_for(
                "debate",
                "Test",
                &["Bob".to_string(), "Alice".to_string()],
                "Mod",
            ),
        )
        .expect("registration order is not identity");
    }

    #[test]
    fn a_mismatch_is_refused_named_and_recoverable() {
        // Resume is automatic, so this check is the only thing between a stale
        // run.json and a session that silently inherits an unrelated
        // conversation. "Cannot resume" alone would say neither which field
        // differs nor what to do about it.
        for (field, value) in [
            ("gameplan", json!("research")),
            ("topic", json!("Something else")),
            ("orchestrator", json!("Other")),
            ("participants", json!(["Alice", "Carol"])),
        ] {
            let mut current = identity();
            current.insert(field.to_string(), value);

            let error = check_identity(&identity(), &current).expect_err("a different run");
            assert!(error.to_string().contains(field), "{error}");
            assert!(error.to_string().contains("Delete the file"), "{error}");
        }
    }
}
