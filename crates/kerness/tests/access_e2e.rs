//! The permission boundary, exercised through a configured session.
//!
//! `access.rs` tests the manager against hand-built policies. What is proved
//! here is the boundary a caller actually meets: the policy handed to
//! [`SessionConfig`](kerness::SessionConfig) is the one the session's own
//! `run_command`, `read_file` and `list_dir` consult, the same one behind the
//! built-in `cmd` tool, and a denial reaches the model as a tool result instead
//! of ending the run.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kerness::access::{AccessPolicy, AccessRequest, ApprovePrompt};
use kerness::provider::ProviderResponse;
use kerness::{Agent, Role, Session, ToolDialect};
use serde_json::json;

use common::{
    config, fenced_tool_call, refusal, RecordingChannel, ScriptedProvider, TempDir, ToolProvider,
};

/// The smallest harness that gets one participant to speak once.
const TOOLED: &str = r#"---
name: tooled
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_turns: 20
  max_rounds: 1
  terminate_on: [DONE]
---

# Tooled
"#;

/// A session whose access policy is *policy* and whose agents never speak.
///
/// Every test below that only asks about the boundary goes through the
/// session's own methods, so what is under test is the policy the caller
/// configured rather than a manager the test built itself.
fn guarded(policy: AccessPolicy) -> Session {
    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let mut settings = config("debate", "Ship it?", provider);
    settings.access_policy = Some(policy);
    Session::new(settings).expect("the gameplan loads")
}

/// An approver that says yes or no to everything and counts what it saw.
struct Standing {
    answer: bool,
    seen: AtomicUsize,
    targets: std::sync::Mutex<Vec<String>>,
}

impl Standing {
    fn new(answer: bool) -> Arc<Standing> {
        Arc::new(Standing {
            answer,
            seen: AtomicUsize::new(0),
            targets: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn count(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().expect("targets lock").clone()
    }
}

impl ApprovePrompt for Standing {
    fn approve(&self, request: &AccessRequest) -> bool {
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.targets
            .lock()
            .expect("targets lock")
            .push(request.target.clone());
        self.answer
    }
}

/// Nothing runs until the policy says so, and the refusal says how to change
/// that — a caller who did not configure access has no other way to find out.
#[test]
fn a_command_is_denied_by_default_and_the_refusal_says_how_to_allow_it() {
    let session = guarded(AccessPolicy::new());

    let message = refusal(session.run_command("echo hello", None, None, "Alice"));
    assert!(message.contains("Approval required"), "{message}");
    assert!(message.contains("approve_prompt"), "{message}");
}

/// The four command allow-lists, each on its own: the program, the exact
/// command, the prefix, and the regex.
#[test]
fn each_command_allow_list_admits_its_own_shape() {
    let cases = [
        (
            "program",
            AccessPolicy {
                allowed_programs: vec!["echo".to_string()],
                ..AccessPolicy::new()
            },
        ),
        (
            "command",
            AccessPolicy {
                allowed_commands: vec!["echo hello".to_string()],
                ..AccessPolicy::new()
            },
        ),
        (
            "prefix",
            AccessPolicy {
                allowed_prefixes: vec!["echo ".to_string()],
                ..AccessPolicy::new()
            },
        ),
        (
            "pattern",
            AccessPolicy {
                allowed_command_patterns: vec![r"^echo\s".to_string()],
                ..AccessPolicy::new()
            },
        ),
    ];

    for (label, policy) in cases {
        let session = guarded(policy);
        let output = session
            .run_command("echo hello", None, None, "Alice")
            .unwrap_or_else(|error| panic!("{label} should have admitted it: {error}"));
        assert_eq!(output.trim(), "hello", "{label}");
    }
}

/// An allow-list is not a category. `allowed_commands` admits the string it was
/// given and nothing adjacent to it.
#[test]
fn an_exact_command_does_not_admit_a_longer_one() {
    let session = guarded(AccessPolicy {
        allowed_commands: vec!["echo hello".to_string()],
        ..AccessPolicy::new()
    });

    let message = refusal(session.run_command("echo hello world", None, None, "Alice"));
    assert!(message.contains("Approval required"), "{message}");
}

/// `set_exec` rebuilds the manager, so a pattern added after construction is in
/// force for the next call and is what `exec()` reports.
#[test]
fn set_exec_replaces_the_patterns_and_takes_effect_at_once() {
    let mut session = guarded(AccessPolicy::new());
    assert!(session.exec().is_empty());

    refusal(session.run_command("echo hello", None, None, "Alice"));

    session.set_exec(vec![r"^echo\s".to_string()]);
    assert_eq!(session.exec(), vec![r"^echo\s".to_string()]);
    let output = session
        .run_command("echo hello", None, None, "Alice")
        .expect("the new pattern admits it");
    assert_eq!(output.trim(), "hello");

    // Replaces rather than appends: the widening is revocable.
    session.set_exec(Vec::new());
    refusal(session.run_command("echo hello", None, None, "Alice"));
}

/// A directory grant covers what is under it and stops at its edge.
#[test]
fn an_allowed_dir_covers_its_files_and_nothing_above_them() {
    let temp = TempDir::new("access");
    temp.write("inside/note.md", "readable");
    let outside = temp.write("outside.md", "not readable");

    let session = guarded(AccessPolicy {
        allowed_dirs: vec![temp.join("inside").to_string_lossy().into_owned()],
        ..AccessPolicy::new()
    });

    assert_eq!(
        session
            .read_file(&temp.join("inside/note.md").to_string_lossy(), "Alice")
            .expect("inside the grant"),
        "readable"
    );
    let message = refusal(session.read_file(&outside.to_string_lossy(), "Alice"));
    assert!(message.contains("Approval required"), "{message}");
}

/// The check resolves before it decides, so climbing out of a granted directory
/// is judged by where the path lands rather than by how it is spelled.
#[test]
fn traversal_out_of_an_allowed_dir_is_denied_after_resolution() {
    let temp = TempDir::new("access");
    temp.write("inside/note.md", "readable");
    temp.write("secret.md", "not readable");

    let session = guarded(AccessPolicy {
        allowed_dirs: vec![temp.join("inside").to_string_lossy().into_owned()],
        ..AccessPolicy::new()
    });

    let climbing = temp.join("inside/../secret.md");
    let message = refusal(session.read_file(&climbing.to_string_lossy(), "Alice"));
    assert!(message.contains("Approval required"), "{message}");
    // The refusal names the resolved path, not the one that was typed.
    assert!(message.contains("secret.md"), "{message}");
    assert!(!message.contains(".."), "{message}");
}

/// A link planted inside a granted directory is judged by its target. Unix only:
/// creating one elsewhere needs a privilege the test cannot assume.
#[cfg(unix)]
#[test]
fn a_symlink_out_of_an_allowed_dir_is_denied() {
    let temp = TempDir::new("access");
    std::fs::create_dir_all(temp.join("inside")).expect("create inside");
    let secret = temp.write("secret.md", "not readable");
    std::os::unix::fs::symlink(&secret, temp.join("inside/link.md")).expect("create symlink");

    let session = guarded(AccessPolicy {
        allowed_dirs: vec![temp.join("inside").to_string_lossy().into_owned()],
        ..AccessPolicy::new()
    });

    let message =
        refusal(session.read_file(&temp.join("inside/link.md").to_string_lossy(), "Alice"));
    assert!(message.contains("Approval required"), "{message}");
    assert!(message.contains("secret.md"), "{message}");
}

/// `allowed_files` is per file, and listing is governed by the same check as
/// reading — a directory is not readable because a file inside it is.
#[test]
fn listing_is_governed_by_the_same_policy_as_reading() {
    let temp = TempDir::new("access");
    let note = temp.write("dir/note.md", "readable");

    let session = guarded(AccessPolicy {
        allowed_files: vec![note.to_string_lossy().into_owned()],
        ..AccessPolicy::new()
    });

    assert_eq!(
        session
            .read_file(&note.to_string_lossy(), "Alice")
            .expect("the file itself is granted"),
        "readable"
    );
    let message = refusal(session.list_dir(&temp.join("dir").to_string_lossy(), "Alice"));
    assert!(message.contains("Approval required"), "{message}");

    let opened = guarded(AccessPolicy {
        allowed_dirs: vec![temp.join("dir").to_string_lossy().into_owned()],
        ..AccessPolicy::new()
    });
    assert_eq!(
        opened
            .list_dir(&temp.join("dir").to_string_lossy(), "Alice")
            .expect("the directory is granted"),
        vec!["note.md".to_string()]
    );
}

/// An unlisted request reaches the approver, carrying who asked and for what;
/// its answer is the decision.
#[test]
fn an_approver_is_consulted_for_anything_unlisted_and_its_answer_stands() {
    let yes = Standing::new(true);
    let allowed = guarded(AccessPolicy {
        approve_prompt: Some(yes.clone()),
        ..AccessPolicy::new()
    });
    let output = allowed
        .run_command("echo hello", None, None, "Alice")
        .expect("the approver said yes");
    assert_eq!(output.trim(), "hello");
    assert_eq!(yes.count(), 1);
    assert_eq!(yes.targets(), vec!["echo hello".to_string()]);

    let no = Standing::new(false);
    let denied = guarded(AccessPolicy {
        approve_prompt: Some(no.clone()),
        ..AccessPolicy::new()
    });
    let message = refusal(denied.run_command("echo hello", None, None, "Alice"));
    assert!(
        message.contains("Approval denied for echo hello"),
        "{message}"
    );
    assert_eq!(no.count(), 1);
}

/// A listed command never reaches the approver: the allow-list is a decision
/// already made, not a hint.
#[test]
fn a_listed_command_is_not_put_to_the_approver() {
    let never = Standing::new(false);
    let session = guarded(AccessPolicy {
        approve_prompt: Some(never.clone()),
        allowed_programs: vec!["echo".to_string()],
        ..AccessPolicy::new()
    });

    session
        .run_command("echo hello", None, None, "Alice")
        .expect("the program is allowed");
    assert_eq!(never.count(), 0);
}

/// The boundary an agent meets: a denied `cmd` call is a tool result it can read
/// and work around, the run continues, and the attempt is reported through the
/// channel with who made it.
#[test]
fn a_denied_tool_call_becomes_a_tool_result_and_a_channel_notice() {
    let temp = TempDir::new("access");
    let path = temp.write("tooled.md", TOOLED);
    let channel = RecordingChannel::new();

    let orchestrator = ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, check the disk."])
        .on("final summary", &["Done."])
        .fallback(&["DONE"])
        .shared();
    let speaker = ToolProvider::new(
        ToolDialect::Text,
        vec![
            ProviderResponse::text(fenced_tool_call("cmd", json!({"command": "rm -rf /"}))),
            ProviderResponse::text("I am not allowed to do that."),
        ],
    )
    .shared();

    let mut settings = config(&path.to_string_lossy(), "Check the disk?", orchestrator);
    settings.channel = Some(channel.clone());
    let mut session = Session::new(settings).expect("the gameplan loads");
    session.add_participant(Agent {
        provider: Some(speaker.clone()),
        ..Agent::new("P0", "gpt-4o")
    });
    session
        .add_orchestrator(Agent {
            role: Role::Orchestrator,
            ..Agent::new("Mod", "gpt-4o")
        })
        .expect("the roster has no orchestrator yet");

    let result = session.run().expect("a denial does not end the run");

    let followup = speaker.calls()[1].text();
    assert!(
        followup.contains("Approval required"),
        "the denial never reached the model: {followup}"
    );
    assert!(
        result
            .history
            .iter()
            .any(|message| message.content.contains("I am not allowed to do that.")),
        "the turn did not finish"
    );
    assert!(
        channel.noted("[Command:denied] P0: rm -rf /"),
        "the attempt was not reported: {:?}",
        channel.system()
    );
}
