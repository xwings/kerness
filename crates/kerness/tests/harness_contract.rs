//! The frontmatter contract, checked against a configured session.
//!
//! A gameplan's YAML is the machine-readable half of the harness, and the claim
//! the project makes about it is that nothing in there is advisory. These tests
//! write gameplans to disk and run sessions against them, so what is proved is
//! the contract a harness author actually writes — not the parser's view of it.

mod common;

use std::sync::Arc;

use kerness::tooling::Arguments;
use kerness::{Agent, Provider, Session, ToolDialect};
use serde_json::json;

use common::{config, refusal, ScriptedProvider, TempDir};

// Gameplan fixtures are raw strings: YAML is whitespace-significant, and an
// escaped literal with `\` line continuations silently eats the indentation
// that decides which mapping a key belongs to.

/// A gameplan that ends immediately, so a test about *configuration* does not
/// also have to script a debate. Its terminator is deliberately not one of the
/// framework's defaults.
const ONE_SHOT: &str = r#"---
name: one-shot
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_rounds: 1
  terminate_on: [DONE]
---

# One shot

End the session.
"#;

/// Declares a tool the host program is not obliged to register.
const NEEDS_TOOLS: &str = r#"---
name: needs-tools
agents:
  orchestrator: true
  participants: {min: 1}
tools: [lookup_price]
loop:
  terminate_on: [DONE]
---

# Needs tools
"#;

/// Two tools are registered; this one permits a single tool by name.
const ONE_TOOL: &str = r#"---
name: one-tool
agents:
  orchestrator: true
  participants: {min: 1}
tools: [echo]
loop:
  max_rounds: 1
  terminate_on: [DONE]
---

# One tool
"#;

const SKILLED: &str = r#"---
name: skilled
agents:
  orchestrator: true
  participants: {min: 1}
skills: [fact-check]
loop:
  max_rounds: 1
  terminate_on: [DONE]
---

# Skilled
"#;

/// A single phase asking for more rounds than the session allows.
const LONG_PHASE: &str = r#"---
name: long-phase
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_turns: 50
  max_rounds: 1
  terminate_on: [DONE]
  phases:
    - name: only
      rounds: 9
---

# Long phase
"#;

const PHASELESS: &str = r#"---
name: phaseless
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_turns: 50
  max_rounds: 2
  terminate_on: [DONE]
---

# Phaseless
"#;

/// Two context sources are registered; this one permits a single source by name.
const ONE_CONTEXT: &str = r#"---
name: one-context
agents:
  orchestrator: true
  participants: {min: 1}
context: [repo_map]
loop:
  max_rounds: 1
  terminate_on: [DONE]
---

# One context
"#;

/// Declares a context source the host program is not obliged to register.
const NEEDS_CONTEXT: &str = r#"---
name: needs-context
agents:
  orchestrator: true
  participants: {min: 1}
context: [deploy_state]
loop:
  terminate_on: [DONE]
---

# Needs context
"#;

const CLAIMS_SKILL: &str = r#"---
name: claims-skill
tools: [Skill]
loop:
  terminate_on: [DONE]
---

# No
"#;

fn echo(_arguments: &Arguments, _actor: &str) -> kerness::Result<String> {
    Ok("echoed".to_string())
}

fn schema() -> serde_json::Value {
    json!({"type": "object", "properties": {}})
}

/// A session on *gameplan*, with *count* participants and one orchestrator.
fn session(gameplan: &str, count: usize, provider: Arc<dyn Provider>) -> Session {
    let mut session =
        Session::new(config(gameplan, "Ship it?", provider)).expect("the gameplan loads");
    for index in 0..count {
        session
            .add_agent(Agent::new(format!("P{index}")).with_model("gpt-4o"))
            .expect("add agent");
    }
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session
}

fn ending(dialect: ToolDialect) -> Arc<ScriptedProvider> {
    ScriptedProvider::new()
        .speaking(dialect)
        .on("final summary", &["Done."])
        .fallback(&["DONE"])
        .shared()
}

/// The bounds are refused before a single provider call, and every problem the
/// caller can fix is listed in one message rather than one per attempt.
#[test]
fn every_unmet_requirement_is_reported_at_once() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session =
        Session::new(config("debate", "Ship it?", provider.clone())).expect("gameplan loads");
    // One participant against `min: 2`, and a name the orchestrator also uses.
    session
        .add_agent(Agent::new("Mod").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let message = session
        .run()
        .expect_err("the roster is illegal")
        .to_string();

    assert!(message.contains("Session does not satisfy the harness"));
    assert!(message.contains("requires at least 2 participant(s), got 1"));
    assert!(message.contains("shares a name with a participant"));
    assert_eq!(
        provider.call_count(),
        0,
        "a configuration error cost a provider call"
    );
}

#[test]
fn too_many_participants_is_refused_and_they_are_named() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    // `debate` allows at most six.
    let mut over = session("debate", 7, provider);

    let message = over.run().expect_err("seven is too many").to_string();
    assert!(message.contains("allows at most 6 participant(s), got 7"));
    assert!(message.contains("P6"));
}

#[test]
fn duplicate_participant_names_are_refused() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(Agent::new("Alice").with_model("gpt-4o"))
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");

    let message = session
        .run()
        .expect_err("two agents cannot answer to one name")
        .to_string();
    assert!(message.contains("duplicate agent name(s): Alice"));
}

/// Tools narrow: a gameplan naming one nobody registered is naming nothing, and
/// the refusal says what *is* registered so the fix is obvious.
#[test]
fn a_declared_tool_that_is_not_registered_is_refused_with_the_list() {
    let temp = TempDir::new("harness");
    let path = temp.write("needs_tools.md", NEEDS_TOOLS);

    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let mut session = session(&path.to_string_lossy(), 1, provider);
    session
        .add_tool("echo", "Echo.", schema(), Arc::new(echo))
        .expect("a fresh name is accepted");

    let message = session
        .run()
        .expect_err("the gameplan wants a tool nobody supplied")
        .to_string();
    assert!(message.contains("requires tool(s) lookup_price"));
    // The built-in tools are registered alongside `echo`, so the list is longer
    // than what this test registered; what matters is that it is *there*.
    assert!(message.contains("Registered:"));
    assert!(message.contains("echo"));
}

/// The narrowing is real at runtime, not only at validation: a tool the gameplan
/// left out is never advertised to the model.
#[test]
fn a_gameplan_narrows_the_registered_tools() {
    let temp = TempDir::new("harness");
    let path = temp.write("one_tool.md", ONE_TOOL);

    let provider = ending(ToolDialect::Openai);
    let mut session = session(&path.to_string_lossy(), 1, provider.clone());
    session
        .add_tool("echo", "Echo.", schema(), Arc::new(echo))
        .expect("a fresh name is accepted");
    session
        .add_tool("secret", "Not for this gameplan.", schema(), Arc::new(echo))
        .expect("a fresh name is accepted");

    session.run().expect("a scripted run cannot fail");

    let offered = provider
        .last_call_for("orchestrator turn")
        .expect("the orchestrator took a turn")
        .tools;
    assert!(offered.contains(&"echo".to_string()));
    assert!(
        !offered.contains(&"secret".to_string()),
        "a tool the gameplan excluded was still offered: {offered:?}"
    );
}

/// Context narrows exactly as tools do, and the refusal says what *is*
/// registered so the fix is obvious.
#[test]
fn a_declared_context_source_that_is_not_registered_is_refused_with_the_list() {
    let temp = TempDir::new("harness");
    let path = temp.write("needs_context.md", NEEDS_CONTEXT);

    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let mut session = session(&path.to_string_lossy(), 1, provider.clone());
    session
        .add_context("repo_map", Arc::new(|_: &str| Ok("src/lib.rs".to_string())))
        .expect("a fresh name is accepted");

    let message = session
        .run()
        .expect_err("the gameplan wants a source nobody supplied")
        .to_string();
    assert!(message.contains("requires context source(s) deploy_state"));
    assert!(message.contains("session.add_context(...)"));
    assert!(message.contains("Registered: repo_map"));
    assert_eq!(
        provider.call_count(),
        0,
        "a configuration error cost a provider call"
    );
}

/// The narrowing is real at runtime, not only at validation: a source the
/// gameplan left out is never rendered, let alone put in a prompt. And what a
/// permitted source returns reaches every agent, under its registered name.
#[test]
fn a_gameplan_narrows_the_registered_context_sources() {
    let temp = TempDir::new("harness");
    let path = temp.write("one_context.md", ONE_CONTEXT);

    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, go.", "DONE"])
        .on("final summary", &["Done."])
        .fallback(&["Read it."])
        .shared();
    let mut session = session(&path.to_string_lossy(), 1, provider.clone());
    session
        .add_context(
            "repo_map",
            // A source is called per agent, so it can answer per agent.
            Arc::new(|agent: &str| Ok(format!("crates/ and bindings/, read by {agent}"))),
        )
        .expect("a fresh name is accepted");
    session
        .add_context(
            "secret",
            Arc::new(|_: &str| Ok("NOT_FOR_THIS_GAMEPLAN".to_string())),
        )
        .expect("a fresh name is accepted");

    session.run().expect("a scripted run cannot fail");

    let prompt = provider
        .last_call_for("orchestrator turn")
        .expect("the orchestrator took a turn")
        .system();
    assert!(prompt.contains("### repo_map"), "{prompt}");
    assert!(prompt.contains("read by Mod"), "{prompt}");
    assert!(
        !prompt.contains("NOT_FOR_THIS_GAMEPLAN"),
        "a source the gameplan excluded still reached the prompt:\n{prompt}"
    );

    let participant = provider
        .last_call_for("turn from P0")
        .expect("the participant took a turn")
        .system();
    assert!(participant.contains("read by P0"), "{participant}");
}

/// A source that fails does so before the first provider call, for the same
/// reason a persona or a tool list does: a configuration error should cost
/// nothing.
#[test]
fn a_context_source_that_fails_stops_the_run_before_any_provider_call() {
    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let mut session = session("debate", 2, provider.clone());
    session
        .add_context(
            "repo_map",
            Arc::new(|_: &str| Err(kerness::Error::session("no such directory"))),
        )
        .expect("a fresh name is accepted");

    let message = session.run().expect_err("the source failed").to_string();
    assert!(message.contains("no such directory"), "{message}");
    assert_eq!(provider.call_count(), 0);
}

/// Skills widen, because a skill is inert instruction text with no handler
/// behind it: the gameplan's list unions with the session's rather than
/// replacing it.
#[test]
fn gameplan_skills_union_with_the_sessions() {
    let temp = TempDir::new("harness");
    let path = temp.write("skilled.md", SKILLED);

    let provider = ending(ToolDialect::Text);
    let mut session = session(&path.to_string_lossy(), 1, provider.clone());
    session.add_skill("challenge").expect("a built-in skill");

    session.run().expect("a scripted run cannot fail");

    let prompt = provider
        .last_call_for("orchestrator turn")
        .expect("the orchestrator took a turn")
        .system();
    assert!(
        prompt.contains("challenge"),
        "the session's skill is missing"
    );
    assert!(
        prompt.contains("fact-check"),
        "the gameplan's skill is missing"
    );
}

/// The runtime builds `Skill` per agent, so a host program shadowing it would
/// disable skill loading with no diagnostic at all.
#[test]
fn the_reserved_tool_name_and_duplicates_are_refused() {
    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let mut session = Session::new(config("debate", "Ship it?", provider)).expect("gameplan loads");

    let Err(reserved) = session.add_tool("Skill", "Mine now.", schema(), Arc::new(echo)) else {
        panic!("the reserved name was accepted");
    };
    assert!(reserved.to_string().contains("reserved tool name"));

    session
        .add_tool("echo", "Echo.", schema(), Arc::new(echo))
        .expect("a fresh name is accepted");
    let Err(duplicate) = session.add_tool("echo", "Echo again.", schema(), Arc::new(echo)) else {
        panic!("a duplicate name was accepted");
    };
    assert!(duplicate.to_string().contains("already registered"));
}

/// The same reservation, one layer earlier: a *gameplan* cannot claim the name.
#[test]
fn a_gameplan_claiming_a_reserved_tool_name_fails_to_load() {
    let temp = TempDir::new("harness");
    let path = temp.write("claims_skill.md", CLAIMS_SKILL);

    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let message = refusal(Session::new(config(
        &path.to_string_lossy(),
        "Ship it?",
        provider,
    )));
    assert!(message.contains("reserved tool(s) Skill"));
}

/// `max_rounds` is a ceiling on any single phase. A phase asking for more than
/// the session allows gets the session's number, not its own.
#[test]
fn a_phase_cannot_outlast_max_rounds() {
    let temp = TempDir::new("harness");
    let path = temp.write("long_phase.md", LONG_PHASE);

    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, go."])
        .on("final summary", &["Done."])
        .fallback(&["My position."])
        .shared();

    let result = session(&path.to_string_lossy(), 1, provider)
        .run()
        .expect("a scripted run cannot fail");

    // One participant, one round: the phase asked for nine and got one.
    assert_eq!(result.rounds_run, 1);
    assert_eq!(result.end_reason, "phases_complete");
    assert_eq!(result.phase_reached, "only");
}

/// A harness that declares no phases is one implicit phase of `max_rounds`
/// rounds — the only shape in which `max_rounds` bounds the whole session.
#[test]
fn a_phaseless_harness_is_bounded_by_max_rounds() {
    let temp = TempDir::new("harness");
    let path = temp.write("phaseless.md", PHASELESS);

    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, go."])
        .on("final summary", &["Done."])
        .fallback(&["My position."])
        .shared();

    let result = session(&path.to_string_lossy(), 1, provider)
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "max_rounds");
    assert_eq!(result.rounds_run, 2);
    assert_eq!(result.phase_reached, "");
}

/// Termination comes from the harness, so a gameplan declaring its own keyword
/// ends on that one and on nothing else.
#[test]
fn only_the_declared_terminator_ends_the_session() {
    let temp = TempDir::new("harness");
    let path = temp.write("one_shot.md", ONE_SHOT);

    let provider = ScriptedProvider::new()
        // `END_SESSION` is the framework's default, and this gameplan does not
        // declare it. Saying it should route nowhere and end nothing, which
        // leaves the orchestrator's turn unparseable and sends it to the retry.
        .on("orchestrator turn", &["END_SESSION"])
        .on("orchestrator retry", &["DONE"])
        .on("final summary", &["Done."])
        .shared();

    let result = session(&path.to_string_lossy(), 1, provider.clone())
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "keyword");
    // The first reply named nobody and ended nothing, so it went to the retry
    // path — which is exactly what a non-terminator should do.
    assert!(provider
        .purposes()
        .iter()
        .any(|purpose| purpose == "orchestrator retry"));
}

/// A harness with no consensus keyword cannot report consensus.
#[test]
fn consensus_is_only_reported_when_the_harness_declares_a_keyword() {
    let temp = TempDir::new("harness");
    let path = temp.write("one_shot.md", ONE_SHOT);

    let provider = ScriptedProvider::new()
        .on("orchestrator turn", &["DONE"])
        .on("final summary", &["Done."])
        .shared();

    let result = session(&path.to_string_lossy(), 1, provider)
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(result.end_reason, "keyword");
    assert!(!result.consensus_reached);
}

/// A gameplan that cannot be loaded says which file, because the caller passed a
/// name and needs to know what that name resolved to.
#[test]
fn a_gameplan_that_is_not_there_names_what_it_looked_for() {
    let provider = ScriptedProvider::new().fallback(&["DONE"]).shared();
    let message = refusal(Session::new(config(
        "no-such-gameplan",
        "Ship it?",
        provider,
    )));
    assert!(message.contains("no-such-gameplan"));
}
