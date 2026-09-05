//! The context ceiling, reached by a session that actually talks too much.
//!
//! `compaction.rs` tests the rewrite against turn lists it builds itself, with
//! a limit handed straight in. What is proved here is the part only a session
//! knows: that the ceiling is the *request*, not the conversation, so the
//! prompt is measured first; that the summarizer is the orchestrator's own
//! provider; and that a summarizer which comes back empty costs the run nothing.

mod common;

use std::sync::Arc;

use kerness::compaction::{estimate_tokens, SUMMARY_PREFIX};
use kerness::{Agent, Session, SessionConfig, SessionResult};
use serde_json::Value;

use common::{config, confine, refusal, RecordingChannel, ScriptedProvider, TempDir};

/// Four rounds of two speakers: enough turns to outgrow a small ceiling.
const LONG: &str = r#"---
name: long
agents:
  orchestrator: true
  participants: {min: 2}
loop:
  max_turns: 20
  max_rounds: 4
  terminate_on: [DONE]
---

# Long
"#;

/// A speech long enough that a handful of them breach the ceiling below.
fn speech(word: &str) -> String {
    format!("{word} ").repeat(400)
}

/// Alternating routes, because a round only closes once both have spoken.
fn routes() -> Vec<&'static str> {
    vec![
        "@Alice, go.",
        "@Bob, go.",
        "@Alice, go.",
        "@Bob, go.",
        "@Alice, go.",
        "@Bob, go.",
        "@Alice, go.",
        "@Bob, go.",
    ]
}

/// Run [`LONG`] under *ceiling* tokens, with *summary* as the summarizer's
/// answer, and hand back the result alongside what the provider saw.
fn run(
    ceiling: usize,
    summary: &str,
    channel: Option<Arc<RecordingChannel>>,
    stepped: bool,
) -> (SessionResult, Arc<ScriptedProvider>, TempDir) {
    let temp = TempDir::new("compaction");
    let path = temp.write("long.md", LONG);
    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &routes())
        .on("compaction", &[summary])
        .on("final summary", &["Done."])
        .fallback(&[&speech("position")])
        .shared();

    let mut settings = config(&path.to_string_lossy(), "Ship it?", provider.clone());
    settings.max_context_tokens = ceiling;
    settings.session_file = Some(temp.str_join("run.json"));
    confine(&mut settings, &temp);
    settings.channel = channel.map(|channel| channel as Arc<dyn kerness::Channel>);

    let mut session = Session::new(settings).expect("the gameplan loads");
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

    let result = if stepped {
        let mut run = session.start(kerness::RunOptions::default()).unwrap();
        loop {
            let before = provider.call_count();
            let step = run.step(kerness::RunInput::Continue).unwrap();
            assert!(
                provider.call_count() - before <= 1,
                "compaction and turn calls need separate steps"
            );
            if let kerness::StepOutcome::Finished { outcome } = step {
                assert_eq!(outcome.reason, kerness::RunReason::Completed);
                assert_eq!(
                    outcome.usage.totals.provider_operations as usize,
                    provider.call_count()
                );
                assert_eq!(
                    outcome
                        .usage
                        .records
                        .iter()
                        .filter(|record| record.purpose == "compaction")
                        .count(),
                    provider
                        .purposes()
                        .iter()
                        .filter(|purpose| purpose.as_str() == "compaction")
                        .count()
                );
                break outcome.result;
            }
        }
    } else {
        session.run().expect("a scripted run cannot fail")
    };
    (result, provider, temp)
}

/// A conversation that outgrows the ceiling is summarized rather than sent, and
/// the summary is asked for from the orchestrator's own provider.
#[test]
fn a_conversation_over_the_ceiling_is_compacted() {
    for stepped in [false, true] {
        let channel = RecordingChannel::new();
        let (_result, provider, _temp) = run(
            2_000,
            "Alice and Bob disagreed.",
            Some(channel.clone()),
            stepped,
        );

        assert!(
            provider
                .purposes()
                .iter()
                .any(|purpose| purpose == "compaction"),
            "no summary was ever requested: {:?}",
            provider.purposes()
        );
        assert!(
            channel.noted("Compacted conversation:"),
            "the compaction was not announced: {:?}",
            channel.system()
        );
    }
}

/// What replaces the dropped turns is labelled, so neither a model nor a reader
/// mistakes a framework-written recap for something an agent said.
#[test]
fn the_summary_replaces_the_dropped_turns_under_its_own_label() {
    let (_result, provider, _temp) = run(2_000, "Alice and Bob disagreed.", None, false);

    let later = provider
        .last_call_for("final summary")
        .expect("the session closed");
    let text = later.text();
    assert!(
        text.contains(SUMMARY_PREFIX),
        "the summary is not labelled: {text}"
    );
    assert!(text.contains("Alice and Bob disagreed."), "{text}");
}

/// The first turn is the topic, and every later turn replies to it. It survives
/// compaction, or the summary and the recent turns discuss something the model
/// can no longer see.
#[test]
fn the_topic_survives_compaction() {
    let (_result, provider, _temp) = run(2_000, "Alice and Bob disagreed.", None, false);

    let later = provider
        .last_call_for("final summary")
        .expect("the session closed");
    assert!(
        later.text().contains("Ship it?"),
        "the anchor turn was dropped"
    );
}

/// A failed summary must leave the conversation intact: trading real turns for
/// an empty directive loses history and buys nothing.
#[test]
fn an_empty_summary_leaves_the_history_alone() {
    let channel = RecordingChannel::new();
    let (result, provider, _temp) = run(2_000, "   ", Some(channel.clone()), false);

    assert!(
        provider
            .purposes()
            .iter()
            .any(|purpose| purpose == "compaction"),
        "the summarizer was never asked"
    );
    assert!(
        !channel.noted("Compacted conversation:"),
        "an empty summary was treated as a compaction"
    );
    assert!(
        result
            .history
            .iter()
            .any(|message| message.sender == "Alice"),
        "history was dropped anyway"
    );
}

/// The count that ends up in the session file is the run's own, so an
/// interrupted session does not forget what it has already summarized.
#[test]
fn the_session_file_records_how_often_it_compacted() {
    let (_result, _provider, temp) = run(2_000, "Alice and Bob disagreed.", None, false);

    let payload: Value = serde_json::from_str(&temp.read("run.json")).expect("the file is JSON");
    assert!(
        payload["compactions"].as_i64().unwrap_or(0) > 0,
        "compactions were not recorded: {payload}"
    );
}

/// Compaction shrinks the conversation only. A ceiling the system prompt alone
/// cannot fit under is refused with what to change, rather than paying for a
/// summary per turn and failing at the API anyway.
#[test]
fn a_ceiling_the_prompt_alone_exceeds_is_refused_with_the_numbers() {
    let temp = TempDir::new("compaction");
    let path = temp.write("long.md", LONG);
    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();

    let mut settings = config(&path.to_string_lossy(), "Ship it?", provider);
    settings.max_context_tokens = 1;
    let mut session = Session::new(settings).expect("the gameplan loads");
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

    let message = refusal(session.run());
    assert!(message.contains("context ceiling of 1"), "{message}");
    assert!(
        message.contains("Compaction shrinks the conversation"),
        "{message}"
    );
}

/// The default ceiling is generous enough that an ordinary run never reaches
/// it, so nothing is summarized and nothing is paid for.
#[test]
fn an_ordinary_run_under_the_default_ceiling_never_compacts() {
    assert_eq!(
        SessionConfig::default().max_context_tokens,
        kerness::session::DEFAULT_MAX_CONTEXT_TOKENS
    );

    let channel = RecordingChannel::new();
    let (_result, provider, _temp) = run(
        kerness::session::DEFAULT_MAX_CONTEXT_TOKENS,
        "unused",
        Some(channel.clone()),
        false,
    );

    assert!(
        !provider
            .purposes()
            .iter()
            .any(|purpose| purpose == "compaction"),
        "a short run paid for a summary"
    );
    assert!(!channel.noted("Compacted conversation:"));
}

/// The heuristic is characters, not bytes: a non-ASCII transcript is not
/// charged twice for the same prose.
#[test]
fn the_estimate_counts_characters_rather_than_bytes() {
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("é".repeat(4).as_str()), 1);
}
