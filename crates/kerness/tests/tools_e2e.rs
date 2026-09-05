//! The tool loop, driven through a real session rather than a bare runner.
//!
//! `agent_runtime`'s own tests build an [`AgentRunner`](kerness::agent_runtime)
//! by hand and hand it a message assembler of two lines. What these prove is the
//! part a caller depends on and those cannot reach: that a session wires the
//! registered tools, the per-agent dialect, the iteration bound and the history
//! setting into that loop, and that a tool failure costs the model a turn's
//! information rather than costing the caller the run.

mod common;

use std::sync::Arc;

use kerness::agent_runtime::{FOLLOWUP_PROMPT, MAX_INVALID_CALLS};
use kerness::provider::ProviderResponse;
use kerness::tooling::Arguments;
use kerness::{Agent, Error, Provider, Session, SessionConfig, ToolDialect};
use serde_json::{json, Value};

use common::{
    config, fenced_tool_call, refusal, tool_call_reply, Call, ScriptedProvider, TempDir,
    ToolProvider,
};

/// One orchestrator turn, one participant turn, then closing. Small enough that
/// the participant's provider sees nothing but its own tool loop.
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

Route to the participant, then close.
"#;

fn lookup_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"ticker": {"type": "string"}},
        "required": ["ticker"],
    })
}

fn lookup(arguments: &Arguments, _actor: &str) -> kerness::Result<String> {
    let ticker = arguments
        .get("ticker")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(format!("{ticker} is at 41.20"))
}

fn boom(_arguments: &Arguments, _actor: &str) -> kerness::Result<String> {
    Err(Error::Value("the exchange is closed".to_string()))
}

/// The orchestrator, which routes once and then says nothing interesting.
fn routing() -> Arc<ScriptedProvider> {
    ScriptedProvider::new()
        .on("orchestrator turn", &["@P0, what is it worth?"])
        .on("final summary", &["Done."])
        .fallback(&["DONE"])
        .shared()
}

/// A session on [`TOOLED`] whose single participant is backed by *speaker*.
///
/// The orchestrator keeps the session's own provider, so every call *speaker*
/// records belongs to the participant's turn and its tool loop.
fn session(temp: &TempDir, speaker: Arc<dyn Provider>, orchestrator: Arc<dyn Provider>) -> Session {
    let path = temp.write("tooled.md", TOOLED);
    let mut session = Session::new(config(
        &path.to_string_lossy(),
        "What is it worth?",
        orchestrator,
    ))
    .expect("the gameplan loads");
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
        .add_tool(
            "lookup",
            "Look up a price.",
            lookup_schema(),
            Arc::new(lookup),
        )
        .expect("a fresh name is accepted");
    session
        .add_tool(
            "boom",
            "Always fails.",
            json!({"type": "object"}),
            Arc::new(boom),
        )
        .expect("a fresh name is accepted");
    session
}

/// Run a session whose participant answers with *replies* under *dialect*, and
/// return every call that participant's provider saw.
fn tool_turn(dialect: ToolDialect, replies: Vec<ProviderResponse>) -> Vec<Call> {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(dialect, replies).shared();
    session(&temp, speaker.clone(), routing())
        .run()
        .expect("a scripted run cannot fail");
    let calls = speaker.calls().clone();
    calls
}

/// A fenced request, then plain text once the result is in view.
fn fenced(name: &str, arguments: Value) -> Vec<ProviderResponse> {
    vec![
        ProviderResponse::text(fenced_tool_call(name, arguments)),
        ProviderResponse::text("Worth it."),
    ]
}

/// Everything one call carried, joined, for asking whether a result got through.
fn seen(call: &Call) -> String {
    call.text()
}

/// The text dialect: the request is a fenced block, the result comes back as an
/// assistant line, and a user turn hands the floor back.
#[test]
fn a_fenced_call_is_dispatched_and_its_result_fed_back() {
    let calls = tool_turn(
        ToolDialect::Text,
        fenced("lookup", json!({"ticker": "KER"})),
    );

    assert_eq!(calls.len(), 2, "one call, one follow-up");
    let followup = seen(&calls[1]);
    assert!(
        followup.contains("[Tool:lookup] KER is at 41.20"),
        "the result never reached the model: {followup}"
    );
    assert!(followup.contains(FOLLOWUP_PROMPT));
    assert!(
        calls[1].tools.is_empty(),
        "text-dialect specs travel in the prompt, not the request"
    );

    let temp = TempDir::new("complete-spec");
    let provider = ToolProvider::new(ToolDialect::Text, fenced("identity", json!({}))).shared();
    let mut run = session(&temp, provider.clone(), routing());
    run.add_tool_spec(
        kerness::ToolSpec::new(
            "identity",
            "Trusted actor",
            json!({"type":"object"}),
            Arc::new(|_: &Arguments, actor: &str| Ok(actor.to_string())),
        )
        .with_actor(),
    )
    .unwrap();
    run.run().unwrap();
    assert!(seen(&provider.calls()[1]).contains("[Tool:identity] P0"));
}

#[test]
fn contextual_tools_keep_actor_scope_and_expire_after_the_invocation() {
    use kerness::{ContextToolSpec, ToolContext};
    use std::sync::Mutex;
    for capped in [false, true] {
        let temp = TempDir::new("tool-context");
        let data = temp.write("worker/data.txt", "scoped data");
        let sibling = temp.write("private.txt", "not in this actor's workspace");
        let provider = ToolProvider::new(
            ToolDialect::Openai,
            vec![
                tool_call_reply("context", json!({"actor":"Mod"}), "model-call"),
                ProviderResponse::text("Done"),
            ],
        )
        .shared();
        let path = temp.write("tooled.md", TOOLED);
        let mut settings = config(&path.to_string_lossy(), "Scoped access", routing());
        common::confine(&mut settings, &temp);
        settings.access_policy.as_mut().unwrap().allowed_commands = vec!["echo scoped".into()];
        settings.memory_write = true;
        let mut session = Session::new(settings).unwrap();
        session
            .add_agent(Agent {
                provider: Some(provider.clone()),
                workspace: Some(temp.str_join("worker")),
                memory: Some(temp.str_join("worker/memory.md")),
                ..Agent::new("P0").with_model("m")
            })
            .unwrap();
        session
            .add_agent(Agent::new("Mod").with_model("m").with_role("orchestrator"))
            .unwrap();
        let captured = Arc::new(Mutex::new(None::<ToolContext>));
        let capture = Arc::clone(&captured);
        let nested = ToolProvider::new(
            ToolDialect::Text,
            vec![ProviderResponse::text("Nested result")],
        )
        .shared();
        let calling_nested = nested.clone();
        session
            .add_contextual_tool(ContextToolSpec::new(
                "context",
                "Use scoped resources",
                json!({"type":"object","properties":{"actor":{"type":"string"}}}),
                Arc::new(move |args: &Arguments, context: &ToolContext| {
                    assert_eq!(args["actor"], json!("Mod"));
                    assert_eq!(context.identity().actor(), "P0");
                    assert_ne!(context.identity().call_id(), "model-call");
                    assert!(!context.identity().run_id().is_empty());
                    assert!(context.identity().turn_id() > 0);
                    assert_eq!(context.read_file(&data.to_string_lossy())?, "scoped data");
                    assert!(context.read_file(&sibling.to_string_lossy()).is_err());
                    assert_eq!(
                        context.run_command("echo scoped", None, None)?.trim(),
                        "scoped"
                    );
                    assert!(context.write_memory("own note")?);
                    assert!(context.read_memory()?.contains("own note"));
                    *capture.lock().unwrap() = Some(context.clone());
                    calling_nested.chat_with_retries(
                        "nested-model",
                        &[],
                        "nested tool",
                        None,
                        kerness::ReasoningEffort::default(),
                    )?;
                    Ok("context worked".into())
                }),
            ))
            .unwrap();
        let mut run = session
            .start(kerness::RunOptions {
                result_validation: kerness::ResultValidation::LegacyCoercion,
                budget: kerness::usage::RunBudget {
                    max_provider_operations: capped.then_some(2),
                    ..kerness::usage::RunBudget::default()
                },
                ..kerness::RunOptions::default()
            })
            .unwrap();
        let mut finished = None;
        for _ in 0..40 {
            match run.step(kerness::RunInput::Continue).unwrap() {
                kerness::StepOutcome::Progress => {}
                kerness::StepOutcome::Finished { outcome } => {
                    finished = Some(outcome);
                    break;
                }
                other => panic!("unexpected wait: {other:?}"),
            }
        }
        let outcome = finished.expect("scripted run completes");
        if capped {
            assert_eq!(
                outcome.reason,
                kerness::RunReason::BudgetExceeded {
                    budget: kerness::usage::BudgetExceeded::ProviderOperations
                }
            );
            assert_eq!(
                nested.call_count(),
                0,
                "tool-internal calls respect the run budget"
            );
            assert_eq!(outcome.usage.totals.provider_operations, 2);
        } else {
            assert_eq!(outcome.reason, kerness::RunReason::Completed);
            assert!(seen(&provider.calls()[1]).contains("context worked"));
            assert_eq!(nested.call_count(), 1);
            let nested_usage: Vec<_> = outcome
                .usage
                .records
                .iter()
                .filter(|record| record.model == "nested-model")
                .collect();
            assert_eq!(
                nested_usage.len(),
                1,
                "provider calls made inside handlers are accounted"
            );
            assert_eq!(nested_usage[0].actor, "P0");
            assert_eq!(nested_usage[0].purpose, "nested tool");
        }
        let handle = captured.lock().unwrap().take().expect("handler ran");
        assert!(handle
            .read_memory()
            .unwrap_err()
            .to_string()
            .contains("expired"));
        assert_eq!(
            handle.identity().actor(),
            "P0",
            "identity remains inspectable"
        );
    }
}

/// OpenAI's shape: the assistant turn is replayed with its `tool_calls`, and the
/// result is its own `role: "tool"` message keyed by call id.
#[test]
fn the_openai_dialect_carries_the_call_and_the_result_natively() {
    let calls = tool_turn(
        ToolDialect::Openai,
        vec![
            tool_call_reply("lookup", json!({"ticker": "KER"}), "c1"),
            ProviderResponse::text("Worth it."),
        ],
    );

    let messages = &calls[1].messages;
    let [.., assistant, result] = messages.as_slice() else {
        panic!("the exchange is replayed: {messages:?}");
    };
    assert_eq!(assistant["tool_calls"][0]["id"], json!("c1"));
    assert_eq!(
        *result,
        json!({"role": "tool", "tool_call_id": "c1", "content": "KER is at 41.20"})
    );
    assert!(
        !seen(&calls[1]).contains(FOLLOWUP_PROMPT),
        "a native result is already a turn and needs no nudge"
    );
}

/// Anthropic's shape: the result is a `tool_result` block inside a user message.
#[test]
fn the_anthropic_dialect_returns_the_result_in_a_user_message() {
    let calls = tool_turn(
        ToolDialect::Anthropic,
        vec![
            tool_call_reply("lookup", json!({"ticker": "KER"}), "tu_1"),
            ProviderResponse::text("Worth it."),
        ],
    );

    let last = calls[1].last();
    assert_eq!(last["role"], json!("user"));
    assert_eq!(last["content"][0]["type"], json!("tool_result"));
    assert_eq!(last["content"][0]["tool_use_id"], json!("tu_1"));
}

/// A session offers what it built plus what the caller registered. `write_memory`
/// is absent because this run may not write memory, which is the default.
#[test]
fn the_built_in_tools_are_offered_alongside_registered_ones() {
    let calls = tool_turn(
        ToolDialect::Openai,
        vec![ProviderResponse::text("Nothing to look up.")],
    );

    let offered = &calls[0].tools;
    for expected in ["cmd", "read_file", "list_dir", "lookup", "boom"] {
        assert!(
            offered.contains(&expected.to_string()),
            "{expected} was not offered: {offered:?}"
        );
    }
    assert!(
        !offered.contains(&"write_memory".to_string()),
        "a read-only run offered the memory writer: {offered:?}"
    );
}

/// A tool the model invented is information for the model, not a failure for the
/// caller: the run continues and the mistake is described in the turn.
#[test]
fn an_unknown_tool_is_answered_rather_than_raised() {
    let calls = tool_turn(ToolDialect::Text, fenced("teleport", json!({})));

    assert!(
        seen(&calls[1]).contains("Unknown tool: teleport"),
        "{}",
        seen(&calls[1])
    );
}

/// Arguments are validated against the declared schema before the handler runs,
/// and the violation is reported the same way — back to the model.
#[test]
fn a_schema_violation_is_answered_rather_than_raised() {
    let calls = tool_turn(ToolDialect::Text, fenced("lookup", json!({})));

    let followup = seen(&calls[1]);
    assert!(followup.contains("lookup:"), "{followup}");
    assert!(
        followup.contains("missing required argument 'ticker'"),
        "{followup}"
    );
}

/// A handler that returns an error is the third case with the same answer.
#[test]
fn a_failing_handler_is_answered_rather_than_raised() {
    let calls = tool_turn(ToolDialect::Text, fenced("boom", json!({})));

    assert!(
        seen(&calls[1]).contains("boom: the exchange is closed"),
        "{}",
        seen(&calls[1])
    );
}

/// An invalid block returns the same "here is the format" text every time, so a
/// model that cannot fix it makes no progress. Nothing else bounds the loop by
/// default, so the framework does.
#[test]
fn a_model_stuck_on_invalid_json_gives_up_instead_of_looping() {
    let calls = tool_turn(
        ToolDialect::Text,
        vec![ProviderResponse::text("```tool_calls\n{bad\n```")],
    );

    assert_eq!(calls.len(), MAX_INVALID_CALLS as usize);
}

/// A caller that would rather bound the loop by count than trust the model to
/// stop asking gets exactly the rounds it configured.
#[test]
fn the_tool_iteration_bound_stops_the_loop() {
    let temp = TempDir::new("tools");
    // One reply, repeated: the model asks for the same tool forever.
    let speaker = ToolProvider::new(
        ToolDialect::Text,
        vec![ProviderResponse::text(fenced_tool_call(
            "lookup",
            json!({"ticker": "KER"}),
        ))],
    )
    .shared();

    let path = temp.write("tooled.md", TOOLED);
    let mut settings = config(&path.to_string_lossy(), "What is it worth?", routing());
    settings.max_tool_iterations = Some(1);
    let mut session = Session::new(settings).expect("the gameplan loads");
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
        .add_tool(
            "lookup",
            "Look up a price.",
            lookup_schema(),
            Arc::new(lookup),
        )
        .expect("a fresh name is accepted");

    session.run().expect("a scripted run cannot fail");

    // The opening call, then one follow-up, then the bound.
    assert_eq!(speaker.call_count(), 2);
}

/// The scratch buffer is the point: an agent sees what another agent *said*, not
/// how it got there.
#[test]
fn tool_exchanges_stay_private_to_the_turn_that_made_them() {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(
        ToolDialect::Text,
        fenced("lookup", json!({"ticker": "KER"})),
    )
    .shared();
    let orchestrator = routing();

    session(&temp, speaker, orchestrator.clone())
        .run()
        .expect("a scripted run cannot fail");

    let closing = orchestrator
        .last_call_for("final summary")
        .expect("the session closed");
    assert!(
        !closing.text().contains("[Tool:lookup]"),
        "another agent's tool exchange leaked into the shared history"
    );
    assert!(
        closing.text().contains("Worth it."),
        "what the participant actually said is missing"
    );
}

/// Opted into, the same exchange enters the shared conversation, and the next
/// agent to be called sees it.
#[test]
fn tool_results_in_history_shows_the_exchange_to_the_next_agent() {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(
        ToolDialect::Text,
        fenced("lookup", json!({"ticker": "KER"})),
    )
    .shared();
    let orchestrator = routing();

    let path = temp.write("tooled.md", TOOLED);
    let mut settings = config(
        &path.to_string_lossy(),
        "What is it worth?",
        orchestrator.clone(),
    );
    settings.tool_results_in_history = true;
    let mut session = Session::new(settings).expect("the gameplan loads");
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
        .add_tool(
            "lookup",
            "Look up a price.",
            lookup_schema(),
            Arc::new(lookup),
        )
        .expect("a fresh name is accepted");

    session.run().expect("a scripted run cannot fail");

    let closing = orchestrator
        .last_call_for("final summary")
        .expect("the session closed");
    assert!(
        closing.text().contains("[Tool:lookup] KER is at 41.20"),
        "the exchange was asked for and did not arrive: {}",
        closing.text()
    );
}

/// Mirroring a native exchange would put one API's message shapes in front of
/// every other agent. That is refused before the first call, not on turn three.
#[test]
fn tool_results_in_history_is_refused_against_a_native_provider() {
    let temp = TempDir::new("tools");
    let path = temp.write("tooled.md", TOOLED);
    let orchestrator = ScriptedProvider::new()
        .speaking(ToolDialect::Anthropic)
        .fallback(&["DONE"])
        .shared();

    let mut settings = config(&path.to_string_lossy(), "What is it worth?", orchestrator);
    settings.tool_results_in_history = true;
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
    assert!(message.contains("TEXT tool dialect only"), "{message}");
    assert!(message.contains("anthropic"), "{message}");
}

/// A session whose config names no provider at all cannot take a turn, and says
/// so with the agent's name rather than a null dereference somewhere downstream.
#[test]
fn an_agent_with_no_provider_anywhere_is_named() {
    let temp = TempDir::new("tools");
    let path = temp.write("tooled.md", TOOLED);
    let settings = SessionConfig {
        gameplan: path.to_string_lossy().into_owned(),
        topic: "What is it worth?".to_string(),
        turn_delay: std::time::Duration::ZERO,
        ..Default::default()
    };

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
    assert!(message.contains("provider"), "{message}");
}

/// The same session, with *tools* declared on the participant.
fn narrowed_session(
    temp: &TempDir,
    speaker: Arc<dyn Provider>,
    tools: Option<Vec<String>>,
) -> Session {
    let path = temp.write("tooled.md", TOOLED);
    let mut session = Session::new(config(
        &path.to_string_lossy(),
        "What is it worth?",
        routing(),
    ))
    .expect("the gameplan loads");
    session
        .add_agent(Agent {
            provider: Some(speaker),
            tools,
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
        .add_tool(
            "lookup",
            "Look up a price.",
            lookup_schema(),
            Arc::new(lookup),
        )
        .expect("a fresh name is accepted");
    session
        .add_tool(
            "boom",
            "Always fails.",
            json!({"type": "object"}),
            Arc::new(boom),
        )
        .expect("a fresh name is accepted");
    session
}

/// A participant that declares its own list is offered that and nothing else.
///
/// Which is what the narrowing is for: under [`ToolDialect::Text`] every offered
/// schema is written into the system prompt, so an agent that names two tools
/// stops paying for the rest of the catalogue on every turn of the session.
#[test]
fn an_agent_declaring_its_own_tools_is_offered_those_alone() {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![ProviderResponse::text("Nothing to look up.")],
    )
    .shared();
    narrowed_session(&temp, speaker.clone(), Some(vec!["lookup".to_string()]))
        .run()
        .expect("a scripted run cannot fail");

    let offered = &speaker.calls()[0].tools;
    assert_eq!(offered, &vec!["lookup".to_string()], "{offered:?}");
}

/// An empty list is a real answer, exactly as it is for skills: this agent
/// argues and calls nothing.
#[test]
fn an_empty_list_leaves_an_agent_with_no_tools_at_all() {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![ProviderResponse::text("Just talk.")],
    )
    .shared();
    narrowed_session(&temp, speaker.clone(), Some(Vec::new()))
        .run()
        .expect("a scripted run cannot fail");

    assert!(speaker.calls()[0].tools.is_empty());
}

/// The narrowing binds the dispatcher too. A model that asks for a tool its
/// agent gave up is answered the way any unknown name is — the tool does not
/// run.
#[test]
fn a_tool_the_agent_gave_up_is_not_callable_either() {
    let temp = TempDir::new("tools");
    let speaker = ToolProvider::new(
        ToolDialect::Openai,
        vec![
            tool_call_reply("boom", json!({}), "c1"),
            ProviderResponse::text("Fine, no."),
        ],
    )
    .shared();
    narrowed_session(&temp, speaker.clone(), Some(vec!["lookup".to_string()]))
        .run()
        .expect("a scripted run cannot fail");

    let followup = seen(&speaker.calls()[1]);
    assert!(followup.contains("Unknown tool: boom"), "{followup}");
    assert!(
        !followup.contains("the exchange is closed"),
        "the handler ran anyway: {followup}"
    );
}

/// Agents narrow. A name the session does not permit is refused before the
/// first provider call, and the refusal names the agent whose list to fix.
#[test]
fn an_agent_cannot_grant_itself_a_tool_the_session_withheld() {
    let temp = TempDir::new("tools");
    let speaker =
        ToolProvider::new(ToolDialect::Openai, vec![ProviderResponse::text("hi")]).shared();
    let message =
        refusal(narrowed_session(&temp, speaker.clone(), Some(vec!["teleport".to_string()])).run());

    assert!(message.contains("P0"), "{message}");
    assert!(message.contains("teleport"), "{message}");
    assert!(
        speaker.calls().is_empty(),
        "a configuration error should cost nothing"
    );
}
