//! A session from `Session::new` to `SessionResult`, driven by a scripted
//! provider.
//!
//! These are the tests that treat the framework as the thing it advertises: a
//! caller writes a gameplan name and a roster, and the declared contract decides
//! who speaks, when it stops, and what comes back. Nothing here reaches past the
//! public API to check an intermediate — the assertions are on the result, the
//! transcript, and what the channel was shown, because those are the three
//! things a dependent actually sees.

mod common;

use std::sync::Arc;

use kerness::orchestrator::FORCED_END_NOTE;
use kerness::provider::ReasoningEffort;
use kerness::{Agent, Provider, Session, SessionConfig};

use common::{config, refusal, RecordingChannel, ScriptedProvider};

/// A closing reply carrying both the prose and the declared result block.
const VERDICT: &str = "They converged on write-through.\n\n\
```json\n{\"consensus\": true, \"summary\": \"They converged on write-through.\"}\n```";

/// Two participants and an orchestrator, which is the smallest roster the
/// `debate` gameplan's `participants: {min: 2}` will accept.
fn debate(provider: Arc<dyn Provider>, channel: Arc<RecordingChannel>) -> Session {
    let mut session = Session::new(SessionConfig {
        channel: Some(channel),
        ..config("debate", "Should the cache be write-through?", provider)
    })
    .expect("the debate gameplan loads");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session
}

/// Alternating routing replies, one per orchestrator turn.
///
/// A single `"@Alice, go."` would be repeated once the script ran out — the
/// scripted provider holds on its last entry — and a round only closes when
/// *every* participant has spoken, so the loop would run to `max_turns` with Bob
/// never called.
fn alternating(count: usize) -> Vec<&'static str> {
    (0..count)
        .map(|turn| {
            if turn % 2 == 0 {
                "@Alice, open the case."
            } else {
                "@Bob, answer that."
            }
        })
        .collect()
}

#[test]
fn a_terminator_ends_the_run_and_says_so() {
    let provider = ScriptedProvider::new()
        .on(
            "orchestrator turn",
            &["@Alice, open the case.", "CONSENSUS_REACHED"],
        )
        .on("final summary", &[VERDICT])
        .fallback(&["Write-through, for the invalidation story."])
        .shared();
    let channel = RecordingChannel::new();

    let result = debate(provider.clone(), channel.clone())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "keyword");
    assert!(result.consensus_reached);
    // Two orchestrator turns and the one participant turn between them. The
    // closing turns are not counted: the loop stops before them, and
    // `turns_completed` is what the harness's `max_turns` is measured against.
    assert_eq!(result.turns_completed, 3);
    assert!(channel.noted("Session ended: CONSENSUS_REACHED"));
}

/// `terminate_on` order decides which keyword means agreement, so the *other*
/// terminator has to end the run without claiming consensus.
#[test]
fn the_non_consensus_terminator_ends_without_agreement() {
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["END_SESSION"])
        .on("final summary", &["No agreement."])
        .shared();

    let result = debate(provider, RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "keyword");
    assert!(!result.consensus_reached);
}

#[test]
fn the_declared_result_fields_come_back_typed() {
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["END_SESSION"])
        .on("final summary", &[VERDICT])
        .shared();

    let result = debate(provider, RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.fields["consensus"], serde_json::json!(true));
    assert_eq!(
        result.fields["summary"],
        serde_json::json!("They converged on write-through.")
    );
    // The block is the machine-readable half; a caller printing the summary
    // should not get JSON in the middle of their prose.
    assert_eq!(result.summary(), "They converged on write-through.");
    assert!(!result.summary().contains("```"));
}

/// A field the closing turn never mentioned still comes back, at its type's
/// default. A partial map would make every caller check for absence.
#[test]
fn an_unreported_result_field_defaults_rather_than_going_missing() {
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["END_SESSION"])
        .on("final summary", &["Nobody wrote any JSON."])
        .shared();

    let result = debate(provider, RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.fields["consensus"], serde_json::json!(false));
    assert_eq!(result.fields["summary"], serde_json::json!(""));
}

/// The declared phase list runs out on its own: four phases, clamped to
/// `max_rounds: 3`, come to five rounds of two participants each.
#[test]
fn the_phase_list_running_out_ends_the_session() {
    let routes = alternating(12);
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &routes)
        .on("final summary", &[VERDICT])
        .fallback(&["My position, unchanged."])
        .shared();

    let result = debate(provider, RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "phases_complete");
    assert_eq!(result.rounds_run, 5);
    assert_eq!(result.phase_reached, "rethink");
    assert_eq!(result.turns_completed, 20);
}

/// Every participant reaches every phase's instruction, whether or not the
/// orchestrator relayed it. This is what makes the phase list structural rather
/// than advisory.
#[test]
fn each_phase_instruction_reaches_the_participant_it_governs() {
    let routes = alternating(12);
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &routes)
        .on("final summary", &[VERDICT])
        .fallback(&["My position."])
        .shared();

    debate(provider.clone(), RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    let asked: String = provider
        .calls()
        .iter()
        .filter(|call| call.purpose.starts_with("turn from"))
        .map(|call| call.text())
        .collect::<Vec<_>>()
        .join("\n");
    for phase in ["think", "argue", "cross_question", "rethink"] {
        assert!(
            asked.contains(&format!("[Phase: {phase}]")),
            "no participant turn was taken under the '{phase}' phase"
        );
    }
}

/// `max_rounds: 0` is the caller saying "skip the debate, give me the verdict".
#[test]
fn zero_rounds_goes_straight_to_the_closing_turn() {
    let provider = ScriptedProvider::new()
        .on("final summary", &[VERDICT])
        .fallback(&["nobody should be asked this"])
        .shared();

    let mut session = Session::new(SessionConfig {
        max_rounds: Some(0),
        ..config("debate", "Ship it?", provider.clone())
    })
    .expect("the debate gameplan loads");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let result = session.run().expect("a scripted run cannot fail");

    assert_eq!(result.turns_completed, 0);
    assert!(result
        .fields
        .get("consensus")
        .is_some_and(|value| value == &serde_json::json!(true)));
    assert!(
        provider.purposes().iter().all(|p| p == "final summary"),
        "a zero-round session asked for something other than the verdict: {:?}",
        provider.purposes()
    );
}

/// The turn budget is a hard stop above the phase list, and a routed reply
/// cannot spend the turn that would take the session past it.
#[test]
fn the_turn_budget_stops_the_loop() {
    let routes = alternating(12);
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &routes)
        .on("final summary", &[VERDICT])
        .fallback(&["My position."])
        .shared();

    let mut session = Session::new(SessionConfig {
        max_turns: Some(3),
        ..config("debate", "Ship it?", provider)
    })
    .expect("the debate gameplan loads");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let result = session.run().expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "max_turns");
    assert_eq!(result.turns_completed, 3);
}

/// An orchestrator that names nobody and ends nothing is re-asked, and the run
/// is closed out rather than spinning.
#[test]
fn an_unroutable_orchestrator_is_retried_and_then_forced() {
    let provider = ScriptedProvider::new()
        .on("final summary", &["Nothing happened."])
        .fallback(&["Let me think about that."])
        .shared();
    let channel = RecordingChannel::new();

    let result = debate(provider.clone(), channel.clone())
        .run()
        .expect("a forced end is not an error");

    assert_eq!(result.end_reason, "forced");
    assert!(channel.noted(FORCED_END_NOTE));
    // The gameplan declares no `orchestrator_retries`, so the harness default
    // of two applies: one turn, then two retries.
    assert_eq!(
        provider
            .purposes()
            .iter()
            .filter(|p| *p == "orchestrator retry")
            .count(),
        2
    );
}

/// `@MEMORY:` is an instruction to the framework, so it is stripped before the
/// text is recorded or delivered — including on a read-only run, where the note
/// itself goes nowhere.
#[test]
fn memory_markers_never_reach_the_transcript_or_the_channel() {
    let provider = ScriptedProvider::new()
        .on(
            "orchestrator turn",
            &["@Alice, open the case.", "END_SESSION"],
        )
        .on("final summary", &["Done."])
        .fallback(&["Write-through.\n@MEMORY: Alice prefers write-through."])
        .shared();
    let channel = RecordingChannel::new();

    let result = debate(provider, channel.clone())
        .run()
        .expect("a scripted run cannot fail");

    let delivered = channel
        .sent()
        .iter()
        .map(|(_, text)| text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(delivered.contains("Write-through."));
    assert!(!delivered.contains("@MEMORY"));
    assert!(!result
        .history
        .iter()
        .any(|message| message.content.contains("@MEMORY")));
}

/// An agent's own provider wins over the session's, which is what lets one
/// session mix backends.
#[test]
fn an_agent_provider_overrides_the_session_one() {
    let session_wide = ScriptedProvider::new()
        .named("session-wide")
        .on(
            "orchestrator turn",
            &["@Alice, open the case.", "END_SESSION"],
        )
        .on("final summary", &["Done."])
        .shared();
    let alices = ScriptedProvider::new()
        .named("alices-own")
        .fallback(&["Write-through, obviously."])
        .shared();

    let mut session = Session::new(config("debate", "Ship it?", session_wide.clone()))
        .expect("the debate gameplan loads");
    session
        .add_agent(Agent {
            provider: Some(alices.clone()),
            ..Agent::new("Alice").with_model("gpt-4o")
        })
        .expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    session.run().expect("a scripted run cannot fail");

    assert_eq!(alices.purposes(), vec!["turn from Alice"]);
    assert!(
        !session_wide
            .purposes()
            .iter()
            .any(|purpose| purpose.contains("Alice")),
        "the session provider took a turn that belonged to Alice's own"
    );
}

/// What the channel showed and what the result carries are the same turns in the
/// same order. A caller watching a run live and a caller reading the transcript
/// afterwards must not see two different sessions.
#[test]
fn the_transcript_and_the_channel_agree() {
    let provider = ScriptedProvider::new()
        .on(
            "orchestrator turn",
            &["@Alice, open the case.", "END_SESSION"],
        )
        .on("final summary", &["Done."])
        .fallback(&["Write-through."])
        .shared();
    let channel = RecordingChannel::new();

    let result = debate(provider, channel.clone())
        .run()
        .expect("a scripted run cannot fail");

    let spoken: Vec<String> = result
        .history
        .iter()
        .filter(|message| message.msg_type != "system")
        .map(|message| message.sender.clone())
        .collect();
    assert_eq!(spoken, channel.senders());
    assert_eq!(spoken, vec!["Mod", "Alice", "Mod", "Mod"]);
}

/// The gameplan body is the orchestrator's manual, and it reaches the model as
/// part of the system prompt rather than as a file path it is told to read.
#[test]
fn the_gameplan_body_reaches_the_orchestrator_prompt() {
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["END_SESSION"])
        .on("final summary", &["Done."])
        .shared();

    debate(provider.clone(), RecordingChannel::new())
        .run()
        .expect("a scripted run cannot fail");

    let prompt = provider
        .last_call_for("orchestrator turn")
        .expect("the orchestrator took a turn")
        .system();
    assert!(prompt.contains("You are running an adversarial debate"));
    assert!(prompt.contains("Ship it?") || prompt.contains("write-through"));
}

#[test]
fn a_run_with_no_provider_names_the_agents_that_needed_one() {
    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Ship it?".to_string(),
        ..Default::default()
    })
    .expect("the debate gameplan loads");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");

    let message = session
        .run()
        .expect_err("no provider is a refusal")
        .to_string();
    assert!(message.contains("No provider configured"));
    assert!(message.contains("Alice") && message.contains("Bob"));
}

#[test]
fn a_run_with_no_topic_no_participants_or_no_orchestrator_is_refused() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();

    let mut topicless =
        Session::new(config("debate", "", provider.clone())).expect("the gameplan loads");
    topicless
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    assert!(topicless
        .run()
        .expect_err("no topic is a refusal")
        .to_string()
        .contains("No topic set"));

    let mut empty =
        Session::new(config("debate", "Ship it?", provider.clone())).expect("the gameplan loads");
    assert!(empty
        .run()
        .expect_err("an empty roster is a refusal")
        .to_string()
        .contains("No participant agents added"));

    let mut leaderless =
        Session::new(config("debate", "Ship it?", provider)).expect("the gameplan loads");
    leaderless
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    leaderless
        .add_agent(Agent::new("Bob").with_model("gpt-4o"))
        .expect("add agent");
    assert!(leaderless
        .run()
        .expect_err("the loop is orchestrator-driven")
        .to_string()
        .contains("No orchestrator agent added"));
}

/// A session takes exactly one orchestrator, and says whose seat it is.
#[test]
fn a_second_orchestrator_is_refused_by_name() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the first is accepted");

    let refusal = session.add_agent(
        Agent::new("Other")
            .with_model("gpt-4o")
            .with_role("orchestrator"),
    );
    // `add_agent` returns `&mut Session` on success, which is not `Debug`, so
    // the error comes out by hand rather than through `expect_err`.
    let Err(error) = refusal else {
        panic!("a second orchestrator was accepted");
    };
    let message = error.to_string();
    assert!(message.contains("already has an orchestrator"));
    assert!(message.contains("Mod"));
}

/// The four things `role` can be, and which chair each one seats.
///
/// The third case is the security-relevant one: prose that *reads* like the
/// built-in name is still prose, and prose never confers the orchestrator's
/// seat. Privilege comes from a file declaring `position: orchestrator`, never
/// from a substring a caller happened to write.
#[test]
fn a_role_seats_an_agent_by_declaration_and_never_by_prose() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");

    let orchestrator_file = kerness::assets::root().join("roles/orchestrator.md");
    session
        .add_agent(Agent::new("Silent").with_model("gpt-4o"))
        .expect("an agent that named no role is accepted")
        .add_agent(
            Agent::new("Prose")
                .with_model("gpt-4o")
                .with_role("orchestrator, but sceptical"),
        )
        .expect("prose is accepted")
        .add_agent(
            Agent::new("Named")
                .with_model("gpt-4o")
                .with_role("participant"),
        )
        .expect("a built-in name is accepted")
        .add_agent(
            Agent::new("Filed")
                .with_model("gpt-4o")
                .with_role(orchestrator_file.display().to_string()),
        )
        .expect("a role file is accepted");

    let seated: Vec<(&str, bool)> = session
        .agents()
        .iter()
        .map(|agent| (agent.name.as_str(), agent.is_orchestrator()))
        .collect();
    assert_eq!(
        seated,
        [
            ("Silent", false),
            ("Prose", false),
            ("Named", false),
            ("Filed", true),
        ]
    );

    // The prose is kept verbatim, because it is that agent's job description
    // and the prompt is the only thing that reads it.
    let prose = &session.agents()[1];
    assert_eq!(prose.role.as_deref(), Some("orchestrator, but sceptical"));
    assert_eq!(
        prose.resolve_role().expect("prose is its own prompt"),
        "orchestrator, but sceptical"
    );
}

/// A role naming a file that is not there is refused at the call that named it,
/// and the refusal lists every directory that was tried.
#[test]
fn a_missing_role_file_is_refused_where_it_was_named() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");

    let refusal = session.add_agent(
        Agent::new("Alice")
            .with_model("gpt-4o")
            .with_role("roles/nonexistent.md"),
    );
    let Err(error) = refusal else {
        panic!("a role file that is not there was accepted");
    };
    let message = error.to_string();
    assert!(message.contains("Alice"), "{message}");
    assert!(message.contains("nonexistent.md"), "{message}");
    assert!(session.agents().is_empty(), "the agent was seated anyway");
}

/// One model and one effort written on the session reach every agent's calls.
///
/// The resolution happens at `run()`, not at `add_agent`, so this writes
/// the defaults *after* the roster is registered: doing it at registration time
/// would freeze whatever the config said in that instant and silently ignore a
/// later change.
#[test]
fn a_session_default_fills_every_agent_that_named_nothing() {
    let provider = ScriptedProvider::new()
        .on(
            "orchestrator turn",
            &[
                "@Alice, open the case.",
                "@Bob, answer that.",
                "CONSENSUS_REACHED",
            ],
        )
        .on("final summary", &[VERDICT])
        .fallback(&["Write-through, for the invalidation story."])
        .shared();

    let mut session = Session::new(SessionConfig {
        model: Some("house/model".to_string()),
        reasoning_effort: ReasoningEffort::Low,
        ..config(
            "debate",
            "Should the cache be write-through?",
            provider.clone(),
        )
    })
    .expect("the debate gameplan loads");
    session.add_agent(Agent::new("Alice")).expect("add agent");
    session
        .add_agent(Agent::new("Bob").with_model("own/model"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Mod").with_role("orchestrator"))
        .expect("the roster has no orchestrator yet");

    session.run().expect("a scripted run cannot fail");

    let models: Vec<String> = provider.calls().iter().map(|c| c.model.clone()).collect();
    assert!(models.contains(&"house/model".to_string()), "{models:?}");
    assert!(models.contains(&"own/model".to_string()), "{models:?}");
    assert!(
        provider
            .calls()
            .iter()
            .all(|c| c.effort == ReasoningEffort::Low),
        "the session's level rides on every call"
    );
}

/// A backend and a model travel together, so an agent that brings one must
/// bring both. The session's model names a model on the *session's* provider,
/// and inheriting it across backends would be a silent wrong answer.
#[test]
fn an_agent_on_a_second_provider_must_name_its_own_model() {
    let house = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let guest = ScriptedProvider::new()
        .named("guest")
        .fallback(&["END_SESSION"])
        .shared();

    let mut session = Session::new(SessionConfig {
        model: Some("house/model".to_string()),
        ..config("debate", "Ship it?", house)
    })
    .expect("the gameplan loads");
    session
        .add_agent(Agent {
            provider: Some(guest),
            ..Agent::new("Alice")
        })
        .expect("add agent");
    session.add_agent(Agent::new("Bob")).expect("add agent");
    session
        .add_agent(Agent::new("Mod").with_role("orchestrator"))
        .expect("the roster has no orchestrator yet");

    let message = refusal(session.run());
    assert!(message.contains("'Alice'"), "{message}");
    assert!(
        message.contains("not inherited across providers"),
        "{message}"
    );
}

/// With no model anywhere, the refusal names both places one could be written.
#[test]
fn a_model_named_nowhere_says_where_to_write_one() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");
    session.add_agent(Agent::new("Alice")).expect("add agent");
    session.add_agent(Agent::new("Bob")).expect("add agent");
    session
        .add_agent(Agent::new("Mod").with_role("orchestrator"))
        .expect("the roster has no orchestrator yet");

    let message = refusal(session.run());
    assert!(message.contains("'Alice'"), "{message}");
    assert!(message.contains("SessionConfig"), "{message}");
}
