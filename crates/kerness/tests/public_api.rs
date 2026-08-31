//! The surface a dependent compiles against, and the assets it ships with.
//!
//! Everything here is reachable only through `kerness::…`. That is the point:
//! a `pub` item that the crate's own unit tests exercise from inside the module
//! can be removed from the public surface without a single test failing, and
//! the break lands on whoever depended on it.
//!
//! The asset half is the Rust counterpart of `python -m kerness.selfcheck`,
//! which proves the same files load — but only for an installed wheel.

mod common;

use std::sync::Arc;

use kerness::agent_runtime::{MAX_INVALID_CALLS, MAX_REPEATED_FAILURES};
use kerness::compaction::{CHARS_PER_TOKEN, COMPACT_TO_FRACTION};
use kerness::exec::DEFAULT_TIMEOUT;
use kerness::gameplan::{list_builtin_gameplans, load_gameplan};
use kerness::harness::RESERVED_TOOL_NAMES;
use kerness::memory::{DEFAULT_KEEP_ENTRIES, DEFAULT_MEMORY_BUDGET, ENTRY_SEPARATOR};
use kerness::persona::{format_persona_for_prompt, list_builtin_personas, load_persona};
use kerness::prompting::MEMORY_STALE_AFTER_DAYS;
use kerness::provider::{
    CLAUDE_BASE_URL, DEFAULT_BACKOFF_SEC, DEFAULT_CLAUDE_MAX_TOKENS, DEFAULT_REQUEST_TIMEOUT_SEC,
    DEFAULT_RETRIES, DEFAULT_TEMPERATURE, DEFAULT_TOP_P, OPENAI_BASE_URL, OPENROUTER_BASE_URL,
};
use kerness::role::{list_builtin_roles, load_role, DEFAULT_ROLE_FILE};
use kerness::session::{DEFAULT_MAX_CONTEXT_TOKENS, OVERFLOW_RETRY_FRACTION};
use kerness::sessionfile::SCHEMA_VERSION;
use kerness::skill::loader::{list_builtin_skills, load_skill};
use kerness::utils::DEFAULT_TERMINATORS;
use kerness::{
    Agent, Channel, ConsoleChannel, Conversation, Error, Memory, Position, Provider,
    ReasoningEffort, RoleConfig, Session, SessionConfig, SessionResult, ToolCall, ToolDialect,
    ToolDispatcher, ToolResult, ToolSpec, VERSION,
};

use common::{RecordingChannel, ScriptedProvider};

/// Every value `ARCHITECTURE.md` publishes in its "Well-Known Constants" table.
/// A doc that names a number is a promise; this is where it is kept.
#[test]
fn the_documented_constants_hold_their_documented_values() {
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(DEFAULT_MAX_CONTEXT_TOKENS, 256_000);
    assert_eq!(CHARS_PER_TOKEN, 4);
    assert_eq!(COMPACT_TO_FRACTION, 0.5);
    assert_eq!(OVERFLOW_RETRY_FRACTION, 0.5);
    assert_eq!(MAX_INVALID_CALLS, 3);
    assert_eq!(MAX_REPEATED_FAILURES, 3);
    assert_eq!(MEMORY_STALE_AFTER_DAYS, 1);
    assert_eq!(DEFAULT_KEEP_ENTRIES, 20);
    assert_eq!(DEFAULT_MEMORY_BUDGET, 2_200);
    assert_eq!(ENTRY_SEPARATOR, "§");
    assert_eq!(DEFAULT_ROLE_FILE, "participant.md");
    assert_eq!(DEFAULT_TERMINATORS, ["CONSENSUS_REACHED", "END_SESSION"]);
    assert_eq!(RESERVED_TOOL_NAMES, ["Skill"]);
    assert_eq!(DEFAULT_TIMEOUT.as_secs(), 60);
    assert_eq!(ReasoningEffort::default(), ReasoningEffort::High);
    assert_eq!(OPENAI_BASE_URL, "https://api.openai.com/v1");
    assert_eq!(OPENROUTER_BASE_URL, "https://openrouter.ai/api/v1");
    assert_eq!(CLAUDE_BASE_URL, "https://api.anthropic.com/v1");
}

/// The request defaults every built-in backend starts from. They are asserted
/// here because the Python constructors declare the same numbers a second time
/// — as their own signature defaults — and a value with two declarations needs
/// one place that says which is right.
#[test]
fn the_shared_request_defaults_hold() {
    assert_eq!(DEFAULT_REQUEST_TIMEOUT_SEC, 60);
    assert_eq!(DEFAULT_RETRIES, 2);
    assert_eq!(DEFAULT_BACKOFF_SEC, 2.0);
    assert_eq!(DEFAULT_TEMPERATURE, 1.0);
    assert_eq!(DEFAULT_TOP_P, 1.0);
    assert_eq!(DEFAULT_CLAUDE_MAX_TOKENS, 4096);
}

/// `VERSION` is what the bindings re-export as `kerness.__version__`, so a
/// caller pinning against it needs it to be the crate version and not a
/// hand-maintained literal that drifted.
#[test]
fn the_crate_version_is_the_one_cargo_built() {
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    assert!(!VERSION.is_empty());
}

/// Names the crate root re-exports. Written as a use of each one rather than a
/// list, because only using it proves it resolves.
#[test]
fn the_root_re_exports_all_resolve() {
    let agent = Agent::new("Alice").with_model("gpt-4o");
    assert_eq!(agent.position, Position::Participant);

    let role = RoleConfig::default();
    assert_eq!(role.position, Position::Participant);

    let conversation = Conversation::new();
    assert!(conversation.is_empty());

    let memory = Memory::new("memory.md");
    assert_eq!(memory.read(), "");

    let result = SessionResult::default();
    assert_eq!(result.summary(), "");

    let call = ToolCall::default();
    assert_eq!(call.name, "");

    let outcome = ToolResult {
        name: "cmd".to_string(),
        content: "ok".to_string(),
        is_error: false,
    };
    assert!(!outcome.is_error);

    assert_eq!(ToolDialect::Text.as_str(), "text");

    let console: Box<dyn Channel> = Box::new(ConsoleChannel::default());
    assert_eq!(console.type_name(), "ConsoleChannel");

    // A dispatcher takes its tool set as a closure rather than a list, so it
    // sees the narrowing a gameplan and an in-flight skill apply.
    let dispatcher = ToolDispatcher::new(Arc::new(Vec::<ToolSpec>::new));
    let missing = dispatcher.execute(&ToolCall::default(), "Alice");
    assert!(missing.is_error);

    let _: Error = Error::session("shape check");
}

/// A session assembles from the public API alone — no `pub(crate)` reach-in,
/// no builder the crate keeps to itself.
#[test]
fn a_session_assembles_from_the_public_api_alone() {
    let provider = ScriptedProvider::new().fallback(&["END_SESSION"]).shared();
    let channel = RecordingChannel::new();

    let mut session = Session::new(SessionConfig {
        gameplan: "debate".to_string(),
        topic: "Ship it?".to_string(),
        provider: Some(provider),
        channel: Some(channel),
        ..Default::default()
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
        .expect("an orchestrator-role agent is accepted");

    assert_eq!(session.agents().len(), 3);
    assert_eq!(session.max_rounds(), 3);
}

/// Enumerated from disk, not from a literal list: an asset added or removed
/// without a matching test change would otherwise pass unnoticed.
#[test]
fn every_bundled_gameplan_loads_and_declares_a_terminator() {
    let names = list_builtin_gameplans();
    assert!(!names.is_empty(), "no gameplans found at all");

    for name in &names {
        let gameplan = load_gameplan(name)
            .unwrap_or_else(|error| panic!("gameplan '{name}' failed to load: {error}"));
        assert!(
            !gameplan.harness.loop_spec.terminate_on.is_empty(),
            "gameplan '{name}' declares no terminate_on, so nothing could end it"
        );
        assert!(
            !gameplan.body.trim().is_empty(),
            "gameplan '{name}' has no body, so the orchestrator gets no manual"
        );
    }
}

#[test]
fn every_bundled_role_loads_and_carries_a_prompt() {
    let roles = list_builtin_roles();
    assert!(!roles.is_empty(), "no roles found at all");
    for name in &roles {
        let role = load_role(&format!("{name}.md"), &[])
            .unwrap_or_else(|error| panic!("role '{name}' failed to load: {error}"));
        assert_eq!(
            &role.name, name,
            "a role's frontmatter must name its own file"
        );
        assert!(
            !role.description.trim().is_empty(),
            "role '{name}' has no description, so the self-check would print nothing for it"
        );
        assert!(
            !role.content.trim().is_empty(),
            "role '{name}' has no body, so an agent wearing it gets no system prompt"
        );
    }
}

#[test]
fn every_bundled_persona_and_skill_loads() {
    let personas = list_builtin_personas();
    assert!(!personas.is_empty(), "no personas found at all");
    for name in &personas {
        let persona = load_persona(&format!("{name}.md"), &[])
            .unwrap_or_else(|error| panic!("persona '{name}' failed to load: {error}"));
        assert!(
            !persona.name.trim().is_empty(),
            "persona '{name}' has no title"
        );
        assert!(
            !format_persona_for_prompt(&persona).trim().is_empty(),
            "persona '{name}' renders to nothing, so it would say nothing in a prompt"
        );
    }

    let skills = list_builtin_skills();
    assert!(!skills.is_empty(), "no skills found at all");
    for name in &skills {
        let skill = load_skill(name)
            .unwrap_or_else(|error| panic!("skill '{name}' failed to load: {error}"));
        assert_eq!(
            &skill.name, name,
            "a skill's frontmatter must name its own directory"
        );
        assert!(
            !skill.description.trim().is_empty(),
            "skill '{name}' has no description, so its index entry would say nothing"
        );
        assert!(
            skill.builtin,
            "a skill listed as built-in must resolve as one"
        );
    }
}

/// A test double implementing `Provider` from outside the crate is the whole
/// reason the trait's supplied methods are supplied rather than private.
#[test]
fn a_provider_written_outside_the_crate_is_a_provider() {
    let provider = ScriptedProvider::new()
        .named("outsider")
        .fallback(&["hello"])
        .shared();

    assert_eq!(provider.name(), "outsider");
    assert_eq!(provider.effective_dialect(), ToolDialect::Text);
    assert!(provider.accepts_tools());
    assert_eq!(
        provider.effective_effort(ReasoningEffort::Max),
        Some(ReasoningEffort::Max)
    );

    let reply = provider
        .chat_with_retries("gpt-4o", &[], "shape check", None, ReasoningEffort::High)
        .expect("the double never fails");
    assert_eq!(reply.content, "hello");
    assert_eq!(provider.purposes(), vec!["shape check"]);
}

/// The level is written as a word in a gameplan or a Python call, so the
/// spelling it accepts is part of the public surface, not an internal detail.
#[test]
fn a_reasoning_effort_is_read_and_written_as_its_name() {
    for (name, level) in [
        ("minimal", ReasoningEffort::Minimal),
        ("low", ReasoningEffort::Low),
        ("medium", ReasoningEffort::Medium),
        ("high", ReasoningEffort::High),
        ("xhigh", ReasoningEffort::XHigh),
        ("max", ReasoningEffort::Max),
    ] {
        assert_eq!(ReasoningEffort::parse(name).unwrap(), level);
        assert_eq!(level.as_str(), name);
        assert_eq!(level.to_string(), name);
    }

    assert_eq!(ReasoningEffort::default(), ReasoningEffort::High);

    // An agent leaves the level unset so the session's can fill it; `effort()`
    // is what a caller reads, and it lands on the default either way.
    let alice = Agent::new("Alice").with_model("gpt-4o");
    assert_eq!(alice.reasoning_effort, None);
    assert_eq!(alice.effort(), ReasoningEffort::High);

    let error = ReasoningEffort::parse("higher").expect_err("a typo is not a level");
    assert_eq!(
        error.to_string(),
        "Unknown reasoning effort 'higher'. Expected 'minimal', 'low', \
         'medium', 'high', 'xhigh', or 'max'."
    );
}
