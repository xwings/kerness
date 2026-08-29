//! Progressive disclosure, seen from where it is paid for.
//!
//! `skill::runtime`'s own tests hold an activation in hand and call `load` on
//! it. What is proved here is the arrangement a session makes around that: the
//! system prompt an agent is actually charged for carries one line per skill and
//! no body, the `Skill` tool that fetches the body is built from that agent's
//! own list, the activation lives exactly as long as the turn, and a skill's
//! `allowed-tools` narrows what the *next* provider call is offered.

mod common;

use std::sync::Arc;

use kerness::provider::ProviderResponse;
use kerness::skill::runtime::SKILL_TOOL_NAME;
use kerness::tooling::Arguments;
use kerness::{Agent, Provider, Session, ToolDialect};
use serde_json::json;

use common::{config, refusal, tool_call_reply, Call, ScriptedProvider, TempDir, ToolProvider};

/// Two rounds of one participant, so a turn boundary falls inside the run.
const SKILLED: &str = r#"---
name: skilled
agents:
  orchestrator: true
  participants: {min: 1}
loop:
  max_turns: 20
  max_rounds: 2
  terminate_on: [DONE]
---

# Skilled

Route to the participant each round.
"#;

/// A line from the built-in `fact-check` body, which no prompt should carry.
const BODY_LINE: &str = "flag inaccuracies";

fn echo(_arguments: &Arguments, _actor: &str) -> kerness::Result<String> {
    Ok("echoed".to_string())
}

fn routing() -> Arc<ScriptedProvider> {
    ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, go."])
        .on("final summary", &["Done."])
        .fallback(&["DONE"])
        .shared()
}

/// A session on [`SKILLED`] whose participant is backed by *speaker*.
///
/// The orchestrator keeps the session's own provider, so every call *speaker*
/// records belongs to the participant and to the skills it was given.
fn session(temp: &TempDir, speaker: Arc<dyn Provider>, skills: &[&str]) -> Session {
    let path = temp.write("skilled.md", SKILLED);
    let mut session =
        Session::new(config(&path.to_string_lossy(), "Ship it?", routing())).expect("it loads");
    session
        .add_agent(Agent {
            provider: Some(speaker),
            ..Agent::new("P0").with_model("gpt-4o")
        })
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session
        .add_tool("echo", "Echo.", json!({"type": "object"}), Arc::new(echo))
        .expect("a fresh name is accepted");
    for name in skills {
        session.add_skill(name).expect("the skill loads");
    }
    session
}

/// Run with *skills* attached and *replies* scripted, and hand back every call
/// the participant's provider saw.
fn turns(dialect: ToolDialect, skills: &[&str], replies: Vec<ProviderResponse>) -> Vec<Call> {
    let temp = TempDir::new("skills");
    let speaker = ToolProvider::new(dialect, replies).shared();
    session(&temp, speaker.clone(), skills)
        .run()
        .expect("a scripted run cannot fail");
    let calls = speaker.calls().clone();
    calls
}

/// Ask for a skill by name, in the native shape.
fn load(name: &str, id: &str) -> ProviderResponse {
    tool_call_reply(SKILL_TOOL_NAME, json!({"name": name}), id)
}

/// Write a skill of *name* into *temp*, and return the path to load it by.
///
/// A `SKILL.md` under a directory of the same name is the shape the loader
/// treats as owning its directory, which is what makes the bundle case below
/// meaningful.
fn write_skill(temp: &TempDir, name: &str, allowed: Option<&str>) -> String {
    let allowed = allowed.map_or(String::new(), |list| format!("allowed-tools: {list}\n"));
    write_skill_keys(temp, name, &allowed)
}

/// The same, with arbitrary extra frontmatter keys.
fn write_skill_keys(temp: &TempDir, name: &str, keys: &str) -> String {
    let text = format!(
        "---\nname: {name}\ndescription: A skill for the tests to load.\n{keys}---\n\n\
         Body of {name}.\n"
    );
    temp.write(&format!("{name}/SKILL.md"), &text)
        .to_string_lossy()
        .into_owned()
}

/// Tool names offered on a call, sorted so the assertion is about membership.
fn offered(call: &Call) -> Vec<String> {
    let mut names = call.tools.clone();
    names.sort();
    names
}

/// The whole point of the index: the agent is charged for a line, not a file.
#[test]
fn only_the_name_and_the_description_reach_the_system_prompt() {
    let calls = turns(
        ToolDialect::Text,
        &["fact-check"],
        vec![ProviderResponse::text("My position.")],
    );

    let prompt = calls[0].system();
    assert!(prompt.contains("## Available Skills"), "{prompt}");
    assert!(
        prompt.contains("- fact-check: Verify factual claims"),
        "{prompt}"
    );
    assert!(
        !prompt.contains(BODY_LINE),
        "the body was inlined into the prompt: {prompt}"
    );
}

/// An agent with nothing to load is offered nothing to load it with — a `Skill`
/// tool whose only outcome is an error is worse than no tool.
#[test]
fn an_agent_with_no_skills_is_offered_no_skill_tool() {
    let calls = turns(
        ToolDialect::Openai,
        &[],
        vec![ProviderResponse::text("My position.")],
    );

    assert!(
        !calls[0].tools.contains(&SKILL_TOOL_NAME.to_string()),
        "{:?}",
        calls[0].tools
    );
    assert!(!calls[0].system().contains("## Available Skills"));
}

/// With skills attached the tool is there, alongside the registered and
/// built-in ones rather than instead of them.
#[test]
fn the_skill_tool_is_offered_beside_the_others() {
    let calls = turns(
        ToolDialect::Openai,
        &["fact-check"],
        vec![ProviderResponse::text("My position.")],
    );

    let names = offered(&calls[0]);
    assert!(names.contains(&SKILL_TOOL_NAME.to_string()), "{names:?}");
    assert!(names.contains(&"echo".to_string()), "{names:?}");
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
}

/// The body arrives as a tool result, in the calling agent's own scratch
/// buffer — and it is gone by the next turn, which is what the design trades
/// for not invalidating a cached system prompt.
#[test]
fn the_body_arrives_for_the_turn_that_asked_and_no_later_one() {
    let calls = turns(
        ToolDialect::Openai,
        &["fact-check"],
        vec![
            load("fact-check", "c1"),
            ProviderResponse::text("Checked."),
            ProviderResponse::text("Still checked."),
        ],
    );

    assert_eq!(
        calls.len(),
        3,
        "one call, its follow-up, then a second turn"
    );
    assert!(
        calls[1].text().contains(BODY_LINE),
        "the body never arrived: {}",
        calls[1].text()
    );
    assert!(
        !calls[2].text().contains(BODY_LINE),
        "the body outlived its turn: {}",
        calls[2].text()
    );
    assert!(
        !calls[2].system().contains(BODY_LINE),
        "the body was folded into the prompt"
    );
}

/// Loading the same skill twice inside one turn is free and says so, rather
/// than paying for the text a second time.
#[test]
fn a_second_load_in_the_same_turn_says_so_instead_of_repeating() {
    let calls = turns(
        ToolDialect::Openai,
        &["fact-check"],
        vec![
            load("fact-check", "c1"),
            load("fact-check", "c2"),
            ProviderResponse::text("Checked."),
        ],
    );

    let second = calls[2].text();
    assert!(
        second.contains("Already loaded earlier in this turn"),
        "{second}"
    );
}

/// A name the agent cannot load is answered with the ones it can, so the model
/// can correct itself instead of guessing again.
#[test]
fn a_skill_the_agent_does_not_have_is_answered_with_the_ones_it_does() {
    let calls = turns(
        ToolDialect::Openai,
        &["fact-check"],
        vec![load("summarize", "c1"), ProviderResponse::text("Fine.")],
    );

    // The `enum` on the tool's own schema is what answers: it is built from this
    // agent's list, so the refusal quotes exactly what it may ask for next.
    let answer = calls[1].text();
    assert!(
        answer.contains("must be one of ['fact-check'], got 'summarize'"),
        "{answer}"
    );
}

/// `allowed-tools` is restrictive: once the skill is active, the follow-up call
/// is offered its list and nothing else — except `Skill`, which stays so the
/// agent can load another one.
#[test]
fn allowed_tools_narrows_what_the_rest_of_the_turn_is_offered() {
    let temp = TempDir::new("skills");
    let narrow = write_skill(&temp, "narrow", Some("[echo]"));

    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![load("narrow", "c1"), ProviderResponse::text("Narrowed.")],
    )
    .shared();
    session(&temp, speaker.clone(), &[&narrow])
        .run()
        .expect("a scripted run cannot fail");

    let calls = speaker.calls().clone();
    assert_eq!(
        offered(&calls[1]),
        vec!["Skill".to_string(), "echo".to_string()],
        "the gate did not narrow the follow-up"
    );
    // The turn after starts a fresh activation, so the gate is gone with it.
    assert!(offered(&calls[2]).contains(&"read_file".to_string()));
}

/// Two skills in one turn union their lists. Intersecting would mean loading a
/// second skill silently disabled the first.
#[test]
fn two_skills_in_one_turn_union_their_gates() {
    let temp = TempDir::new("skills");
    let narrow = write_skill(&temp, "narrow", Some("[echo]"));
    let wider = write_skill(&temp, "wider", Some("[read_file]"));

    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![
            load("narrow", "c1"),
            load("wider", "c2"),
            ProviderResponse::text("Both."),
        ],
    )
    .shared();
    session(&temp, speaker.clone(), &[&narrow, &wider])
        .run()
        .expect("a scripted run cannot fail");

    let calls = speaker.calls().clone();
    assert_eq!(offered(&calls[1]), vec!["Skill", "echo"]);
    assert_eq!(
        offered(&calls[2]),
        vec![
            "Skill".to_string(),
            "echo".to_string(),
            "read_file".to_string()
        ],
        "the second skill replaced the first instead of joining it"
    );
}

/// A skill loaded from a path is not a reason to open the directory it sits in.
/// The manifest says what is there and says the reading will need approval.
#[test]
fn a_skill_from_a_path_lists_its_bundle_without_opening_it() {
    let temp = TempDir::new("skills");
    let bundled = write_skill(&temp, "bundled", None);
    temp.write("bundled/scripts/run.sh", "echo hi\n");

    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![load("bundled", "c1"), ProviderResponse::text("Read it.")],
    )
    .shared();
    session(&temp, speaker.clone(), &[&bundled])
        .run()
        .expect("a scripted run cannot fail");

    let manifest = speaker.calls()[1].text();
    assert!(manifest.contains("access was not granted"), "{manifest}");
    assert!(manifest.contains("scripts"), "{manifest}");
}

/// `Agent::skills` selects exactly, so one agent's list is not the session's and
/// not the other agent's.
#[test]
fn an_agents_own_list_replaces_the_sessions() {
    let temp = TempDir::new("skills");
    let path = temp.write("skilled.md", SKILLED);
    let watcher = ScriptedProvider::new().fallback(&["My position."]).shared();

    let mut session =
        Session::new(config(&path.to_string_lossy(), "Ship it?", routing())).expect("it loads");
    session
        .add_agent(Agent {
            provider: Some(watcher.clone()),
            skills: Some(vec!["challenge".to_string()]),
            ..Agent::new("P0").with_model("gpt-4o")
        })
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session.add_skill("fact-check").expect("the skill loads");
    session.run().expect("a scripted run cannot fail");

    let prompt = watcher
        .last_call_for("turn from P0")
        .expect("the participant spoke")
        .system();
    assert!(prompt.contains("- challenge:"), "{prompt}");
    assert!(
        !prompt.contains("- fact-check:"),
        "an explicit list inherited anyway: {prompt}"
    );
}

/// `allowed-tools: []` is an answer, not an omission: a pure-instruction skill
/// that wants the agent to stop calling things gets exactly that, and `Skill`
/// survives so the turn is not a dead end.
#[test]
fn an_empty_allowed_tools_leaves_only_the_skill_tool() {
    let temp = TempDir::new("skills");
    let silent = write_skill(&temp, "silent", Some("[]"));

    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![load("silent", "c1"), ProviderResponse::text("Quiet.")],
    )
    .shared();
    session(&temp, speaker.clone(), &[&silent])
        .run()
        .expect("a scripted run cannot fail");

    assert_eq!(offered(&speaker.calls()[1]), vec![SKILL_TOOL_NAME]);
}

/// A gameplan naming `tools: [echo]` that carries a skill whose instructions
/// drive `read_file`. `requires-tools` is the one additive direction: without
/// it the skill is prose about a tool the agent was never offered.
#[test]
fn a_required_tool_comes_back_past_the_gameplans_own_list() {
    let temp = TempDir::new("skills");
    let path = temp.write(
        "narrowed.md",
        &SKILLED.replace("---\n\n# Skilled", "tools: [echo]\n---\n\n# Skilled"),
    );
    let reader = write_skill_keys(&temp, "reader", "requires-tools: [read_file]\n");

    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![load("reader", "c1"), ProviderResponse::text("Read it.")],
    )
    .shared();
    let mut session =
        Session::new(config(&path.to_string_lossy(), "Ship it?", routing())).expect("it loads");
    session
        .add_agent(Agent {
            provider: Some(speaker.clone()),
            ..Agent::new("P0").with_model("gpt-4o")
        })
        .expect("add agent");
    session
        .add_agent(
            Agent::new("Mod")
                .with_model("gpt-4o")
                .with_role("orchestrator"),
        )
        .expect("the roster has no orchestrator yet");
    session
        .add_tool("echo", "Echo.", json!({"type": "object"}), Arc::new(echo))
        .expect("a fresh name is accepted");
    session.add_skill(&reader).expect("the skill loads");
    session.run().expect("a scripted run cannot fail");

    let calls = speaker.calls().clone();
    // Before the load the gameplan's list is the whole story.
    assert_eq!(offered(&calls[0]), vec!["Skill", "echo"]);
    assert_eq!(offered(&calls[1]), vec!["Skill", "echo", "read_file"]);
}

/// The requirement is checked against what the *caller registered*, before the
/// first provider call. A run that discovers the gap mid-turn spends tokens to
/// arrive at an agent apologising for a capability nobody shipped.
#[test]
fn a_required_tool_nobody_registered_is_refused_before_the_run() {
    let temp = TempDir::new("skills");
    let needy = write_skill_keys(&temp, "needy", "requires-tools: [write_file]\n");
    let speaker = ScriptedProvider::new().fallback(&["DONE"]).shared();

    let mut session = session(&temp, speaker, &[&needy]);
    let message = refusal(session.run());
    assert!(
        message.contains("'needy' requires the tool 'write_file'"),
        "{message}"
    );
    assert!(message.contains("nobody registered"), "{message}");
    assert!(message.contains("add_tool"), "{message}");
    // The registered names are listed, so the author can see the near-miss.
    assert!(message.contains("echo"), "{message}");
}
