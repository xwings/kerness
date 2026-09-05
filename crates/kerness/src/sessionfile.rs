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

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::conversation::{Message, Turn};
use crate::error::{Error, Result};
use crate::pyfmt;

/// Bumped whenever the shape below changes incompatibly.
///
/// A file from a version this build does not know is rejected rather than
/// guessed at: the fields it is missing would otherwise resume as silent
/// defaults.
pub const SCHEMA_VERSION: i64 = 2;

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
    /// Loop counters, phase position, and optional version 2 `runtime`
    /// continuation; see `OrchestratorLoop::snapshot` and `SessionRun`.
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
/// Written through an exclusively created sibling file and renamed. Existing
/// temporary files and symlinks are left untouched; a collision gets a new
/// name. A failed write or rename removes only the temporary file this save
/// created, leaving the previous snapshot intact. The file is synced before
/// rename; on Unix the containing directory is synced afterward. A directory
/// sync failure is reported even though replacement has already occurred.
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
        "turns": snapshot.turns,
        "transcript": snapshot.transcript,
        "loop": snapshot.loop_state,
        "compactions": snapshot.compactions,
    });
    validate_payload(payload.as_object().expect("snapshot is an object"), path)?;

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let mut tmp = path.with_file_name(&tmp_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = loop {
        match options.open(&tmp) {
            Ok(file) => break file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let mut name = tmp_name.clone();
                name.push(format!(
                    ".{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                tmp = path.with_file_name(name);
            }
            Err(err) => return Err(Error::Io(format!("{}: {err}", tmp.display()))),
        }
    };
    let written = file
        .write_all((pyfmt::json_dumps_indent2(&payload) + "\n").as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let result = written
        .and_then(|()| fs::rename(&tmp, path))
        .and_then(|()| sync_parent(path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|err| Error::Io(format!("{}: {err}", path.display())))
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
    if !matches!(version.as_i64(), Some(1 | SCHEMA_VERSION)) {
        return Err(Error::Session(format!(
            "Session file {} has schema version {}; this build of kerness \
             writes version {SCHEMA_VERSION} and reads versions 1 and {SCHEMA_VERSION}. Delete it to start \
             fresh, or pass a different session_file path.",
            path.display(),
            pyfmt::repr(version)
        )));
    }
    validate_payload(&payload, path)?;

    Ok(Some(SessionSnapshot {
        identity: payload["identity"]
            .as_object()
            .expect("validated identity")
            .clone(),
        turns: Vec::deserialize(&payload["turns"]).expect("validated turns"),
        transcript: Vec::deserialize(&payload["transcript"]).expect("validated transcript"),
        loop_state: payload["loop"]
            .as_object()
            .expect("validated loop state")
            .clone(),
        compactions: payload["compactions"]
            .as_i64()
            .expect("validated compactions count"),
    }))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Check the shared envelope before any conversion. The runtime and scheduler
/// owners validate their own version 2 continuation objects when resuming.
fn validate_payload(payload: &Map<String, Value>, path: &Path) -> Result<()> {
    let invalid = |field: &str| {
        Error::session(format!(
        "Session file {} has an invalid or missing {field}. Delete it to start fresh, or pass a different session_file path.",
        path.display()
    ))
    };
    let fields = [
        "version",
        "identity",
        "turns",
        "transcript",
        "loop",
        "compactions",
    ];
    if fields.iter().any(|field| !payload.contains_key(*field))
        || payload
            .keys()
            .any(|field| !fields.contains(&field.as_str()))
    {
        return Err(invalid("snapshot envelope field"));
    }
    let identity = payload["identity"]
        .as_object()
        .ok_or_else(|| invalid("identity"))?;
    if identity.len() != IDENTITY_FIELDS.len()
        || ["gameplan", "topic", "orchestrator"]
            .iter()
            .any(|field| !identity.get(*field).is_some_and(Value::is_string))
        || !identity.get("participants").is_some_and(string_array)
    {
        return Err(invalid("identity fields"));
    }
    for (field, names) in [
        (
            "turns",
            &["role", "speaker", "content", "round_idx", "msg_type"][..],
        ),
        (
            "transcript",
            &["sender", "content", "round_idx", "msg_type"][..],
        ),
    ] {
        let records = payload[field].as_array().ok_or_else(|| invalid(field))?;
        for (index, value) in records.iter().enumerate() {
            let valid = value.as_object().is_some_and(|record| {
                record.len() == names.len()
                    && names.iter().all(|name| {
                        record.get(*name).is_some_and(|value| {
                            if *name == "round_idx" {
                                nonnegative(value)
                            } else {
                                value.is_string()
                            }
                        })
                    })
            });
            if !valid {
                return Err(invalid(&format!("{field}[{index}] record")));
            }
        }
    }
    if !nonnegative(&payload["compactions"]) {
        return Err(invalid("compactions count"));
    }
    let state = payload["loop"]
        .as_object()
        .ok_or_else(|| invalid("loop state"))?;
    for (field, value) in state {
        let valid = match field.as_str() {
            "turn_count" => nonnegative(value),
            "phases" => value.as_object().is_some_and(|phases| {
                phases.iter().all(|(name, value)| match name.as_str() {
                    "index" | "round_in_phase" | "rounds_run" => nonnegative(value),
                    "pending" => string_array(value),
                    "exhausted" => value.is_boolean(),
                    _ => false,
                })
            }),
            "runtime" | "scheduler" => {
                payload["version"].as_i64() == Some(SCHEMA_VERSION) && value.is_object()
            }
            _ => false,
        };
        if !valid {
            return Err(invalid(&format!("loop.{field}")));
        }
    }
    Ok(())
}

fn nonnegative(value: &Value) -> bool {
    value.as_i64().is_some_and(|count| count >= 0)
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::conversation::Conversation;
    use crate::testing::TempDir;

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
        // render() folds speaker into content and drops msg_type. A snapshot that
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

        // Valid version-1 boundaries remain readable without inventing a
        // suspended action. The run owner migrates that boundary on start.
        let mut payload: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        payload["version"] = json!(1);
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(load_snapshot(&path).unwrap(), Some(loaded.clone()));
        assert!(!loaded.loop_state.contains_key("runtime"));

        // The common envelope preserves the runtime owner's continuation
        // verbatim, including identities and side-effect progress.
        let mut suspended = loaded;
        suspended.loop_state.insert(
            "runtime".to_string(),
            json!({
                "run_id": "run-1", "turn_id": "turn-2",
                "pending_approval": {"id": "approval-2", "call_id": "call-2"},
                "completed_results": [{"call_id": "call-1", "result": "done"}],
                "usage": crate::usage::UsageLedger::default()
            }),
        );
        save_snapshot(&path, &suspended).unwrap();
        assert_eq!(load_snapshot(&path).unwrap(), Some(suspended));
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

        save_snapshot(&path, &SessionSnapshot::new(identity())).unwrap();
        let valid: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let mut malformed = Vec::new();
        for field in ["identity", "turns", "transcript", "loop", "compactions"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            malformed.push(missing);
        }
        for (field, bad) in [
            ("identity", json!({})),
            (
                "turns",
                json!([{"role": "assistant", "speaker": "a", "content": 4, "round_idx": 1, "msg_type": "turn"}]),
            ),
            (
                "transcript",
                json!([{"sender": "a", "content": "x", "round_idx": -1, "msg_type": "turn"}]),
            ),
            ("loop", json!({"turn_count": "1"})),
            ("loop", json!({"phases": {"pending": [1]}})),
            ("loop", json!({"phases": {"index": -1}})),
            ("loop", json!({"runtime": []})),
            ("loop", json!({"extra": true})),
            ("compactions", json!(-1)),
            ("extra", json!(true)),
        ] {
            let mut value = valid.clone();
            value[field] = bad;
            malformed.push(value);
        }
        for mut value in malformed {
            for version in [1, SCHEMA_VERSION] {
                value["version"] = json!(version);
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
                let error = load_snapshot(&path).expect_err("malformed snapshots cannot default");
                assert!(error.to_string().contains("run.json"), "{value}: {error}");
            }
        }
        for continuation in ["runtime", "scheduler"] {
            let mut value = valid.clone();
            value["version"] = json!(1);
            value["loop"][continuation] = json!({});
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                load_snapshot(&path).is_err(),
                "v1 cannot contain suspended work"
            );
        }
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

        let path = dir.join("run.json");
        let occupied = dir.join("run.json.tmp");
        let mut replacement = SessionSnapshot::new(identity());
        replacement.compactions = 7;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let outside = TempDir::new("snapshot-outside");
            let victim = outside.write("private.txt", "untouched");
            std::os::unix::fs::symlink(&victim, &occupied).unwrap();
            save_snapshot(&path, &replacement).expect("a temporary symlink is skipped");
            assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
            assert!(fs::symlink_metadata(&occupied)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(!fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(load_snapshot(&path).unwrap(), Some(replacement.clone()));
            fs::remove_file(&occupied).unwrap();
        }

        fs::write(&occupied, "unrelated temporary file").unwrap();
        save_snapshot(&path, &replacement).expect("an occupied temporary name is skipped");
        assert_eq!(
            fs::read_to_string(&occupied).unwrap(),
            "unrelated temporary file"
        );
        assert_eq!(load_snapshot(&path).unwrap(), Some(replacement));

        let blocked = dir.join("directory.json");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("keep.txt"), "keep").unwrap();
        assert!(save_snapshot(&blocked, &SessionSnapshot::new(identity())).is_err());
        assert_eq!(
            fs::read_to_string(blocked.join("keep.txt")).unwrap(),
            "keep"
        );
        let mut left: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["directory.json", "nested", "run.json", "run.json.tmp"]
        );
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
