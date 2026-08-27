//! One agent turn: assemble, call, run tools until the model stops asking.
//!
//! [`AgentRunner`] keeps calling until the model stops requesting tools, so a
//! model that needs two rounds of tool use — read a file, then run a command
//! based on what it found — does it inside a single turn.
//!
//! The loop runs in a private scratch buffer seeded from the shared
//! conversation, and only the final text goes back into the conversation. That
//! isolation is the point: the buffer holds tool exchanges in one provider's
//! message shape, and no other agent should have to read them.

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::error::Result;
use crate::logging;
use crate::provider::{Provider, ProviderResponse};
use crate::tooling::{parse_tool_calls, ToolCall, ToolSpec, INVALID_CALL};
use crate::toolkit::ToolDispatcher;
use crate::toolschema::{render_assistant_turn, render_tool_result, ToolDialect};

/// Prompt sent after tool results so the model continues with them in view.
pub const FOLLOWUP_PROMPT: &str = "Tool results are available above. Continue.";

/// How many consecutive unparseable tool-call blocks to tolerate before giving
/// up on the turn.
///
/// Every other tool result carries new information, so looping on it is the
/// model making progress; an invalid block returns the same "here is the
/// format" text every time, so a model that cannot produce valid JSON will emit
/// the same reply forever. With the tool-iteration bound unset that is an
/// unbounded loop against a paid API.
pub const MAX_INVALID_CALLS: u32 = 3;

/// Builds the message list an agent is called with, from a rendered history.
type MessagesFor<'a> = Box<dyn Fn(&Agent, &[Value], &str) -> Result<Vec<Value>> + 'a>;

/// Sink for the tool messages a turn produces.
type RecordExchange<'a> = Box<dyn FnMut(&Value) + 'a>;

/// Runs a single agent turn, including its private tool loop.
pub struct AgentRunner<'a> {
    agent: &'a Agent,
    provider: &'a dyn Provider,
    messages_for: MessagesFor<'a>,
    dispatcher: &'a ToolDispatcher,
    base_prompt: String,
    max_tool_iterations: Option<u32>,
    record: Option<RecordExchange<'a>>,
    tools_for: Option<Box<dyn Fn() -> Vec<ToolSpec> + 'a>>,
    native_tools: bool,
}

impl<'a> AgentRunner<'a> {
    /// Prepare one turn for *agent*, backed by *provider*.
    ///
    /// *base_prompt* is what the system prompt is assembled from; the tool loop
    /// is unbounded and nothing native is advertised until
    /// [`AgentRunner::with_max_tool_iterations`] and
    /// [`AgentRunner::with_tools`] say otherwise.
    pub fn new(
        agent: &'a Agent,
        provider: &'a dyn Provider,
        messages_for: impl Fn(&Agent, &[Value], &str) -> Result<Vec<Value>> + 'a,
        dispatcher: &'a ToolDispatcher,
        base_prompt: impl Into<String>,
    ) -> Self {
        AgentRunner {
            agent,
            provider,
            messages_for: Box::new(messages_for),
            dispatcher,
            base_prompt: base_prompt.into(),
            max_tool_iterations: None,
            record: None,
            tools_for: None,
            native_tools: false,
        }
    }

    /// Stop the tool loop after *limit* rounds.
    pub fn with_max_tool_iterations(mut self, limit: u32) -> Self {
        self.max_tool_iterations = Some(limit);
        self
    }

    /// Send every tool message to *record* as well as the scratch buffer, for a
    /// session configured to keep them in the shared history.
    pub fn with_record(mut self, record: impl FnMut(&Value) + 'a) -> Self {
        self.record = Some(Box::new(record));
        self
    }

    /// Advertise these specs natively.
    ///
    /// Only consulted when the provider's effective dialect is not
    /// [`ToolDialect::Text`] — under text the same specs already reached the
    /// model through the system prompt.
    pub fn with_tools(mut self, tools_for: impl Fn() -> Vec<ToolSpec> + 'a) -> Self {
        self.tools_for = Some(Box::new(tools_for));
        self
    }

    /// Take one turn and return the agent's final text.
    ///
    /// *history* is the rendered conversation the agent starts from, *purpose*
    /// names the turn in provider logging and retries, and *instruction* is an
    /// optional user-role prompt appended for this turn. A provider that fails
    /// costs the turn its text, not the run.
    pub fn run(
        &mut self,
        history: &[Value],
        purpose: &str,
        instruction: Option<&str>,
    ) -> Result<String> {
        let dialect = self.provider.effective_dialect();
        // Under text the specs already reached the model through the system
        // prompt, so there is nothing to send.
        self.native_tools = self.tools_for.is_some() && dialect != ToolDialect::Text;

        let mut scratch: Vec<Value> = history.to_vec();
        if let Some(instruction) = instruction {
            scratch.push(json!({"role": "user", "content": instruction}));
        }

        let mut response = match self.chat(&scratch, purpose) {
            Ok(response) => response,
            Err(error) if error.is_provider() => return Ok(no_response(purpose)),
            Err(error) => return Err(error),
        };

        let mut iterations = 0;
        let mut invalid = 0;
        loop {
            let calls = calls_from(&response, dialect);
            if calls.is_empty() {
                return Ok(response.content);
            }
            if self
                .max_tool_iterations
                .is_some_and(|limit| iterations >= limit)
            {
                return Ok(response.content);
            }

            invalid = if all_invalid(&calls) { invalid + 1 } else { 0 };
            if invalid >= MAX_INVALID_CALLS {
                logging::warning(&format!(
                    "Giving up on {purpose} after {invalid} unparseable tool-call blocks"
                ));
                return Ok(response.content);
            }

            self.append(&mut scratch, render_assistant_turn(dialect, &response));
            for call in &calls {
                let result = self.dispatcher.execute(call, &self.agent.name);
                self.append(&mut scratch, render_tool_result(dialect, call, &result));
            }
            if dialect == ToolDialect::Text {
                // The native dialects end on their own result message; text
                // renders results as assistant turns, so it needs a user turn
                // to hand the floor back.
                self.append(
                    &mut scratch,
                    json!({"role": "user", "content": FOLLOWUP_PROMPT}),
                );
            }

            iterations += 1;
            response = match self.chat(&scratch, &format!("{purpose} (tool followup)")) {
                Ok(response) => response,
                Err(error) if error.is_provider() => return Ok(no_response(purpose)),
                Err(error) => return Err(error),
            };
        }
    }

    fn append(&mut self, scratch: &mut Vec<Value>, message: Value) {
        if let Some(record) = self.record.as_mut() {
            record(&message);
        }
        scratch.push(message);
    }

    fn chat(&self, scratch: &[Value], purpose: &str) -> Result<ProviderResponse> {
        let messages = (self.messages_for)(self.agent, scratch, &self.base_prompt)?;
        let tools = match (self.native_tools, self.tools_for.as_ref()) {
            (true, Some(tools_for)) => Some(tools_for()),
            _ => None,
        };
        self.provider
            .chat_with_retries(&self.agent.model, &messages, purpose, tools.as_deref())
    }
}

/// Read tool calls from wherever this dialect puts them.
///
/// Native calls are authoritative when present. Falling back to the fence
/// parser regardless is deliberate: a model told about tools natively may still
/// narrate one in prose, and refusing to read that would strand the turn.
fn calls_from(response: &ProviderResponse, dialect: ToolDialect) -> Vec<ToolCall> {
    if dialect != ToolDialect::Text && !response.tool_calls.is_empty() {
        return response.tool_calls.clone();
    }
    parse_tool_calls(&response.content)
}

/// Whether a round produced nothing but unparseable blocks.
fn all_invalid(calls: &[ToolCall]) -> bool {
    calls.iter().all(|call| call.name == INVALID_CALL)
}

fn no_response(purpose: &str) -> String {
    logging::warning(&format!("Provider error for {purpose}"));
    format!("[No response from model for {purpose}]")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::error::Error;
    use crate::provider::ProviderBase;
    use crate::tooling::Arguments;

    fn call_block(name: &str) -> String {
        format!(
            "```tool_calls\n\
             {{\"tool_calls\":[{{\"id\":\"c1\",\"type\":\"function\",\
             \"function\":{{\"name\":\"{name}\",\"arguments\":\"{{}}\"}}}}]}}\n\
             ```\n"
        )
    }

    fn spec(name: &str, output: &'static str) -> ToolSpec {
        ToolSpec::new(
            name,
            format!("{name} tool"),
            json!({"type": "object", "properties": {}}),
            Arc::new(move |_: &Arguments, _: &str| Ok(output.to_string())),
        )
    }

    fn ping() -> Vec<ToolSpec> {
        vec![spec("ping", "pong")]
    }

    /// The agent taking the turn and the dispatcher behind it.
    ///
    /// Returned as owned values because the runner borrows both, so they have
    /// to outlive it in the test's own scope.
    fn fixture(tools: Vec<ToolSpec>) -> (Agent, ToolDispatcher) {
        (
            Agent::new("Alice", "m"),
            ToolDispatcher::new(Arc::new(move || tools.clone())),
        )
    }

    /// The plainest possible assembly: a system message, then the scratch.
    fn messages_for(_agent: &Agent, history: &[Value], base: &str) -> Result<Vec<Value>> {
        let mut messages = vec![json!({"role": "system", "content": base})];
        messages.extend(history.iter().cloned());
        Ok(messages)
    }

    fn contents(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .map(|message| message["content"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// One call as the provider saw it.
    #[derive(Clone)]
    struct Recorded {
        messages: Vec<Value>,
        tools: Option<Vec<ToolSpec>>,
        purpose: String,
    }

    /// Replies in order, repeating the last one, and records every call.
    ///
    /// `None` stands for a backend that is down, which is the only failure the
    /// runner is meant to absorb.
    struct MockProvider {
        base: ProviderBase,
        dialect: ToolDialect,
        replies: Vec<Option<ProviderResponse>>,
        calls: Mutex<Vec<Recorded>>,
    }

    impl MockProvider {
        fn new(dialect: ToolDialect, replies: Vec<Option<ProviderResponse>>) -> Self {
            MockProvider {
                base: ProviderBase::new(0, 0.0, None),
                dialect,
                replies,
                calls: Mutex::new(Vec::new()),
            }
        }

        /// Plain text replies over the fenced protocol.
        fn text(replies: &[&str]) -> Self {
            MockProvider::new(
                ToolDialect::Text,
                replies
                    .iter()
                    .map(|reply| Some(ProviderResponse::text(*reply)))
                    .collect(),
            )
        }

        fn call(&self, index: usize) -> Recorded {
            self.calls.lock().expect("lock")[index].clone()
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("lock").len()
        }
    }

    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "MockProvider"
        }

        fn base(&self) -> &ProviderBase {
            &self.base
        }

        fn tool_dialect(&self) -> ToolDialect {
            self.dialect
        }

        fn chat(
            &self,
            _model: &str,
            _messages: &[Value],
            _tools: Option<&[ToolSpec]>,
        ) -> Result<ProviderResponse> {
            unreachable!("the runner goes through chat_with_retries")
        }

        fn chat_with_retries(
            &self,
            _model: &str,
            messages: &[Value],
            purpose: &str,
            tools: Option<&[ToolSpec]>,
        ) -> Result<ProviderResponse> {
            let mut calls = self.calls.lock().expect("lock");
            calls.push(Recorded {
                messages: messages.to_vec(),
                tools: tools.map(<[ToolSpec]>::to_vec),
                purpose: purpose.to_string(),
            });
            let index = (calls.len() - 1).min(self.replies.len() - 1);
            self.replies[index]
                .clone()
                .ok_or_else(|| Error::provider("down"))
        }
    }

    #[test]
    fn a_reply_without_tool_calls_is_returned_as_is() {
        let provider = MockProvider::text(&["My view."]);
        let (agent, dispatcher) = fixture(Vec::new());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), "My view.");
    }

    #[test]
    fn the_instruction_is_appended_after_the_history() {
        let provider = MockProvider::text(&["ok"]);
        let (agent, dispatcher) = fixture(Vec::new());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");
        let history = [json!({"role": "assistant", "content": "[Mod] @Alice, go."})];

        runner
            .run(&history, "turn", Some("Speak now."))
            .expect("a turn");

        assert_eq!(
            provider.call(0).messages,
            vec![
                json!({"role": "system", "content": "BASE"}),
                history[0].clone(),
                json!({"role": "user", "content": "Speak now."}),
            ]
        );
    }

    #[test]
    fn tool_output_is_fed_back_and_the_final_text_returned() {
        // The instruction rides along into the follow-up. Rebuilding the
        // follow-up from shared history alone would drop it, and the model
        // would come back from its tool call not knowing what it was asked.
        let provider = MockProvider::text(&[call_block("ping").as_str(), "Got pong."]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        let text = runner
            .run(&[], "orchestrator turn", Some("Open the debate."))
            .expect("a turn");
        assert_eq!(text, "Got pong.");

        let followup = provider.call(1);
        assert!(followup
            .messages
            .contains(&json!({"role": "assistant", "content": "[Tool:ping] pong"})));
        let said = contents(&followup.messages);
        assert_eq!(said.last().expect("a follow-up prompt"), FOLLOWUP_PROMPT);
        assert!(said.contains(&"Open the debate.".to_string()), "{said:?}");
        assert_eq!(followup.purpose, "orchestrator turn (tool followup)");
    }

    #[test]
    fn the_loop_runs_more_than_one_round() {
        // A model that reads a file and then runs a command based on what it
        // found needs two rounds inside one turn.
        let provider = MockProvider::text(&[
            call_block("read").as_str(),
            call_block("ping").as_str(),
            "Done after two rounds.",
        ]);
        let (agent, dispatcher) = fixture(vec![spec("ping", "pong"), spec("read", "contents")]);
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn", None).expect("a turn"),
            "Done after two rounds."
        );
        assert_eq!(provider.call_count(), 3);
    }

    #[test]
    fn an_error_result_is_fed_back_rather_than_re_prompted() {
        // A bad call is the model's own to correct on the next iteration.
        let provider = MockProvider::text(&[
            call_block("teleport").as_str(),
            "Sorry, I'll use ping instead.",
        ]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn", None).expect("a turn"),
            "Sorry, I'll use ping instead."
        );
        let fed_back = contents(&provider.call(1).messages).join("\n");
        assert!(
            fed_back.contains("[ToolError] Unknown tool: teleport"),
            "{fed_back}"
        );
    }

    #[test]
    fn a_model_stuck_on_invalid_json_does_not_loop_forever() {
        // An invalid block returns the same text every time, so a model that
        // cannot fix it makes no progress — and the tool-iteration bound is
        // unset by default, so nothing else stops the loop.
        let provider = MockProvider::text(&["```tool_calls\n{bad\n```"]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn", None).expect("a turn"),
            "```tool_calls\n{bad\n```"
        );
        // Each bad response costs one call; the third trips the bound.
        assert_eq!(provider.call_count(), MAX_INVALID_CALLS as usize);
    }

    #[test]
    fn a_recovering_model_is_not_penalised_for_an_earlier_bad_block() {
        // The counter tracks *consecutive* failures — one bad block followed by
        // a good one must not count toward the next run of bad ones.
        let provider = MockProvider::text(&[
            "```tool_calls\n{bad\n```",
            call_block("ping").as_str(),
            "Recovered.",
        ]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), "Recovered.");
    }

    #[test]
    fn the_tool_iteration_bound_stops_the_loop() {
        let block = call_block("ping");
        let provider = MockProvider::text(&[block.as_str()]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE")
            .with_max_tool_iterations(2);

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), block);
        // 1 opening call + 2 followups, then the bound stops it.
        assert_eq!(provider.call_count(), 3);
    }

    #[test]
    fn recording_captures_the_whole_exchange_when_asked() {
        let provider = MockProvider::text(&[call_block("ping").as_str(), "done"]);
        let (agent, dispatcher) = fixture(ping());
        let recorded: Mutex<Vec<Value>> = Mutex::new(Vec::new());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE")
            .with_record(|message| recorded.lock().expect("lock").push(message.clone()));

        runner.run(&[], "turn", None).expect("a turn");

        assert_eq!(
            contents(&recorded.lock().expect("lock")),
            [
                call_block("ping"),
                "[Tool:ping] pong".to_string(),
                FOLLOWUP_PROMPT.to_string()
            ]
        );
    }

    #[test]
    fn a_failed_opening_call_returns_a_placeholder() {
        let provider = MockProvider::new(ToolDialect::Text, vec![None]);
        let (agent, dispatcher) = fixture(Vec::new());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn from Alice", None).expect("a turn"),
            "[No response from model for turn from Alice]"
        );
    }

    #[test]
    fn a_failed_followup_returns_a_placeholder() {
        let provider = MockProvider::new(
            ToolDialect::Text,
            vec![Some(ProviderResponse::text(call_block("ping"))), None],
        );
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn", None).expect("a turn"),
            "[No response from model for turn]"
        );
    }

    #[test]
    fn a_native_call_round_trips_through_the_response_not_the_text() {
        // The follow-up replays the assistant turn and its result — both native
        // APIs 400 without it — and adds no nudge of its own: a tool result is
        // already a turn, which is what text needs the extra prompt to fake.
        let provider = MockProvider::new(
            ToolDialect::Openai,
            vec![
                Some(ProviderResponse {
                    tool_calls: vec![ToolCall::new("ping", Arguments::new()).with_id("c1")],
                    stop_reason: "tool_calls".to_string(),
                    ..ProviderResponse::default()
                }),
                Some(ProviderResponse::text("Got pong.")),
            ],
        );
        let (agent, dispatcher) = fixture(ping());
        let mut runner =
            AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE").with_tools(ping);

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), "Got pong.");

        let followup = provider.call(1).messages;
        let [.., assistant, result] = followup.as_slice() else {
            panic!("the exchange is replayed: {followup:?}");
        };
        assert_eq!(assistant["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(
            *result,
            json!({"role": "tool", "tool_call_id": "c1", "content": "pong"})
        );
        assert!(
            !followup
                .iter()
                .any(|message| message.to_string().contains(FOLLOWUP_PROMPT)),
            "{followup:?}"
        );
    }

    #[test]
    fn schemas_are_sent_natively_and_never_under_text() {
        // Under text the specs already reached the model in the prompt.
        for (dialect, sent) in [(ToolDialect::Openai, true), (ToolDialect::Text, false)] {
            let provider = MockProvider::new(dialect, vec![Some(ProviderResponse::text("hi"))]);
            let (agent, dispatcher) = fixture(ping());
            let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE")
                .with_tools(ping);
            runner.run(&[], "turn", None).expect("a turn");

            let names: Option<Vec<String>> = provider
                .call(0)
                .tools
                .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect());
            assert_eq!(names, sent.then(|| vec!["ping".to_string()]), "{dialect:?}");
        }
    }

    #[test]
    fn anthropic_results_ride_in_a_user_message() {
        let provider = MockProvider::new(
            ToolDialect::Anthropic,
            vec![
                Some(ProviderResponse {
                    tool_calls: vec![ToolCall::new("ping", Arguments::new()).with_id("tu_1")],
                    ..ProviderResponse::default()
                }),
                Some(ProviderResponse::text("done")),
            ],
        );
        let (agent, dispatcher) = fixture(ping());
        let mut runner =
            AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE").with_tools(ping);
        runner.run(&[], "turn", None).expect("a turn");

        let messages = provider.call(1).messages;
        let last = messages.last().expect("a result message");
        assert_eq!(last["role"], json!("user"));
        assert_eq!(last["content"][0]["tool_use_id"], json!("tu_1"));
    }

    #[test]
    fn a_fenced_call_still_works_under_a_native_dialect() {
        // A natively-equipped model may still narrate a call in prose.
        let provider = MockProvider::new(
            ToolDialect::Openai,
            vec![
                Some(ProviderResponse::text(call_block("ping"))),
                Some(ProviderResponse::text("done")),
            ],
        );
        let (agent, dispatcher) = fixture(ping());
        let mut runner =
            AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE").with_tools(ping);

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), "done");
    }
}
