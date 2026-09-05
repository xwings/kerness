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
//!
//! The caller may limit tool rounds. [`MAX_INVALID_CALLS`] and
//! [`MAX_REPEATED_FAILURES`] always apply and end only the current turn.

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::error::{Error, Result};
use crate::logging;
use crate::provider::{Provider, ProviderResponse};
use crate::tooling::{parse_tool_calls, ToolCall, ToolSpec, INVALID_CALL};
use crate::toolkit::{ToolDispatcher, ToolResult};
use crate::toolschema::{render_assistant_turn, render_tool_result, ToolDialect};

/// Prompt sent after tool results so the model continues with them in view.
pub const FOLLOWUP_PROMPT: &str = "Tool results are available above. Continue.";

/// How many consecutive unparseable tool-call blocks to tolerate before giving
/// up on the turn.
pub const MAX_INVALID_CALLS: u32 = 3;

/// How many times an all-error tool round may repeat the previous round's
/// failures, word for word, before giving up on the turn.
pub const MAX_REPEATED_FAILURES: u32 = 3;

/// Why an agent turn stopped. Legacy callers continue to receive its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnReason {
    Completed,
    ToolIterations,
    InvalidCalls,
    RepeatedFailures,
    ProviderFailure,
}

/// The owned continuation of one agent turn, including every completed tool
/// result. Providers, handlers, and prompt callbacks are bound by the driver.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurn {
    scratch: Vec<Value>,
    history_len: usize,
    purpose: String,
    dialect: ToolDialect,
    max_tool_iterations: Option<u32>,
    iterations: u32,
    invalid: u32,
    repeated: u32,
    previous: Option<Vec<ToolResult>>,
    calls: Vec<ToolCall>,
    next_call: usize,
    results: Vec<ToolResult>,
    response_content: String,
    final_text: Option<String>,
    reason: Option<TurnReason>,
    failure: Option<String>,
    recorded: Vec<Value>,
}

impl AgentTurn {
    pub fn new(
        history: &[Value],
        purpose: &str,
        instruction: Option<&str>,
        dialect: ToolDialect,
        max_tool_iterations: Option<u32>,
    ) -> Self {
        let mut scratch = history.to_vec();
        if let Some(instruction) = instruction {
            scratch.push(json!({"role": "user", "content": instruction}));
        }
        Self {
            scratch,
            history_len: history.len(),
            purpose: purpose.to_string(),
            dialect,
            max_tool_iterations,
            iterations: 0,
            invalid: 0,
            repeated: 0,
            previous: None,
            calls: Vec::new(),
            next_call: 0,
            results: Vec::new(),
            response_content: String::new(),
            final_text: None,
            reason: None,
            failure: None,
            recorded: Vec::new(),
        }
    }

    pub fn scratch(&self) -> &[Value] {
        &self.scratch
    }

    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// The next unexecuted call. Inspect it before approval or journalling;
    /// only accepting its result advances this cursor.
    pub fn pending_call(&self) -> Option<&ToolCall> {
        if self.is_complete() {
            None
        } else {
            self.calls.get(self.next_call)
        }
    }

    pub fn is_complete(&self) -> bool {
        self.final_text.is_some()
    }

    pub fn text(&self) -> Option<&str> {
        self.final_text.as_deref()
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn reason(&self) -> Option<TurnReason> {
        self.reason
    }

    /// Messages newly appended by this continuation. Draining consumes the
    /// outbox, so a later snapshot cannot emit the same records again.
    pub fn take_recorded(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.recorded)
    }

    /// Replace the shared-history prefix after compaction, keeping the turn's
    /// instruction and complete tool exchanges. No completed tool is replayed.
    pub fn replace_history(&mut self, history: &[Value]) -> Result<()> {
        self.validate()?;
        self.scratch
            .splice(..self.history_len, history.iter().cloned());
        self.history_len = history.len();
        Ok(())
    }

    pub fn snapshot(&self) -> Value {
        serde_json::to_value(self).expect("agent turn contains only JSON-compatible state")
    }

    pub fn from_snapshot(snapshot: &Value) -> Result<Self> {
        let turn: Self = serde_json::from_value(snapshot.clone())
            .map_err(|err| Error::session(format!("Invalid agent turn snapshot: {err}")))?;
        turn.validate()?;
        Ok(turn)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.history_len > self.scratch.len()
            || self.final_text.is_some() != self.reason.is_some()
            || self.next_call > self.calls.len()
            || self.results.len() != self.next_call
            || self
                .results
                .iter()
                .zip(&self.calls)
                .any(|(result, call)| result.name != call.name)
        {
            return Err(Error::session(
                "Invalid agent turn snapshot: inconsistent tool or history position",
            ));
        }
        // Each finished tool round retains at least an assistant message and
        // one result, even when the shared-history prefix was compacted.
        if u64::from(self.iterations) > ((self.scratch.len() - self.history_len) / 2) as u64
            || self.invalid > MAX_INVALID_CALLS
            || self.repeated > MAX_REPEATED_FAILURES
            || (!self.is_complete()
                && (self.invalid == MAX_INVALID_CALLS || self.repeated == MAX_REPEATED_FAILURES))
            || self.max_tool_iterations.is_some_and(|limit| {
                self.iterations > limit
                    || (self.iterations == limit && self.pending_call().is_some())
            })
        {
            return Err(Error::session(
                "Invalid agent turn snapshot: inconsistent loop guard counters",
            ));
        }
        Ok(())
    }

    fn append(&mut self, message: Value) {
        self.recorded.push(message.clone());
        self.scratch.push(message);
    }

    fn accept_response(&mut self, response: &ProviderResponse, dialect: ToolDialect) {
        self.dialect = dialect;
        self.response_content = response.content.clone();
        self.calls = calls_from(response, dialect);
        self.next_call = 0;
        self.results.clear();
        if self.calls.is_empty()
            || self
                .max_tool_iterations
                .is_some_and(|limit| self.iterations >= limit)
        {
            self.final_text = Some(response.content.clone());
            self.reason = Some(if self.calls.is_empty() {
                TurnReason::Completed
            } else {
                TurnReason::ToolIterations
            });
            return;
        }
        self.invalid = if self.calls.iter().all(|call| call.name == INVALID_CALL) {
            self.invalid.saturating_add(1)
        } else {
            0
        };
        if self.invalid >= MAX_INVALID_CALLS {
            logging::warning(&format!(
                "Giving up on {} after {} unparseable tool-call blocks",
                self.purpose, self.invalid
            ));
            self.final_text = Some(response.content.clone());
            self.reason = Some(TurnReason::InvalidCalls);
            return;
        }
        self.append(render_assistant_turn(dialect, response));
    }

    /// Commit one tool result without executing another call or contacting a
    /// provider. Native call/result batches remain private until all calls have
    /// a result, at which point the provider may be advanced again.
    pub fn accept_tool_result(&mut self, result: ToolResult) -> Result<()> {
        self.validate()?;
        let call = self
            .pending_call()
            .cloned()
            .ok_or_else(|| Error::session("This agent turn has no pending tool call"))?;
        if result.name != call.name {
            return Err(Error::session(format!(
                "Tool result for '{}' does not answer pending call '{}'",
                result.name, call.name
            )));
        }
        self.append(render_tool_result(self.dialect, &call, &result));
        self.results.push(result);
        self.next_call += 1;
        if self.next_call != self.calls.len() {
            return Ok(());
        }
        let stuck = self.results.iter().all(|result| result.is_error)
            && self.previous.as_deref() == Some(self.results.as_slice());
        self.repeated = if stuck {
            self.repeated.saturating_add(1)
        } else {
            0
        };
        self.previous = Some(self.results.clone());
        if self.repeated >= MAX_REPEATED_FAILURES {
            logging::warning(&format!(
                "Giving up on {} after {} repeats of the same failing tool calls",
                self.purpose, self.repeated
            ));
            self.final_text = Some(self.response_content.clone());
            self.reason = Some(TurnReason::RepeatedFailures);
            return Ok(());
        }
        if self.dialect == ToolDialect::Text {
            self.append(json!({"role": "user", "content": FOLLOWUP_PROMPT}));
        }
        self.iterations = self.iterations.saturating_add(1);
        Ok(())
    }
}

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
    strict_errors: bool,
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
            strict_errors: false,
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

    /// Prepare owned state without contacting the provider.
    pub fn start(&self, history: &[Value], purpose: &str, instruction: Option<&str>) -> AgentTurn {
        AgentTurn::new(
            history,
            purpose,
            instruction,
            self.provider.effective_dialect(),
            self.max_tool_iterations,
        )
    }

    /// Propagate provider failures so a session driver can return a typed
    /// terminal outcome. The legacy runner absorbs them by default.
    pub fn with_strict_errors(mut self) -> Self {
        self.strict_errors = true;
        self
    }

    /// Make one logical provider request. Provider-owned retries remain inside
    /// that invocation; no tool is executed by this method. An overflow leaves
    /// the continuation untouched so compaction can retry without replaying tools.
    pub fn advance(&mut self, turn: &mut AgentTurn) -> Result<Option<ProviderResponse>> {
        turn.validate()?;
        if turn.is_complete() {
            return Ok(None);
        }
        if turn.pending_call().is_some() {
            return Err(Error::session(
                "Accept the pending tool result before advancing the provider",
            ));
        }
        let purpose = if turn.iterations == 0 {
            turn.purpose.clone()
        } else {
            format!("{} (tool followup)", turn.purpose)
        };
        let response = match self.chat(turn.scratch(), &purpose) {
            Ok(response) => response,
            Err(error)
                if !self.strict_errors && error.is_provider() && !error.is_context_overflow() =>
            {
                turn.failure = Some(error.to_string());
                turn.final_text = Some(no_response(&turn.purpose));
                turn.reason = Some(TurnReason::ProviderFailure);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        turn.accept_response(&response, self.provider.effective_dialect());
        self.record_pending(turn);
        Ok(Some(response))
    }

    /// Drive the same owned continuation to completion, dispatching each tool
    /// through the registered dispatcher before advancing the provider again.
    pub fn run(
        &mut self,
        history: &[Value],
        purpose: &str,
        instruction: Option<&str>,
    ) -> Result<String> {
        let mut turn = self.start(history, purpose, instruction);
        loop {
            if let Some(text) = turn.text() {
                return Ok(text.to_string());
            }
            if let Some(call) = turn.pending_call().cloned() {
                let result = self.dispatcher.execute(&call, &self.agent.name);
                turn.accept_tool_result(result)?;
                self.record_pending(&mut turn);
            } else if let Err(error) = self.advance(&mut turn) {
                // Legacy callers cannot carry this private continuation
                // into a retry. Absorb followup overflow as before rather
                // than inviting a retry that would replay completed tools.
                if !self.strict_errors && turn.iterations > 0 && error.is_provider() {
                    return Ok(no_response(purpose));
                }
                return Err(error);
            }
        }
    }

    fn record_pending(&mut self, turn: &mut AgentTurn) {
        if let Some(record) = self.record.as_mut() {
            for message in turn.take_recorded() {
                record(&message);
            }
        }
    }

    fn chat(&self, scratch: &[Value], purpose: &str) -> Result<ProviderResponse> {
        let messages = (self.messages_for)(self.agent, scratch, &self.base_prompt)?;
        let tools = match (self.provider.effective_dialect(), self.tools_for.as_ref()) {
            (ToolDialect::Text, _) | (_, None) => None,
            (_, Some(tools_for)) => Some(tools_for()),
        };
        crate::usage::observe_provider_call(
            self.provider.name(),
            self.agent.model_name(),
            purpose,
            || {
                self.provider.chat_with_retries(
                    self.agent.model_name(),
                    &messages,
                    purpose,
                    tools.as_deref(),
                    self.agent.effort(),
                )
            },
        )
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

fn no_response(purpose: &str) -> String {
    logging::warning(&format!("Provider error for {purpose}"));
    format!("[No response from model for {purpose}]")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::error::Error;
    use crate::provider::{ProviderBase, ReasoningEffort};
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
            Agent::new("Alice").with_model("m"),
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
        effort: ReasoningEffort,
    }

    /// Replies in order, repeating the last one, and records every call.
    ///
    /// `None` stands for a backend that is down, which is the only failure the
    /// runner is meant to absorb.
    struct MockProvider {
        base: ProviderBase,
        dialect: ToolDialect,
        replies: Vec<Option<ProviderResponse>>,
        failure: Error,
        calls: Mutex<Vec<Recorded>>,
    }

    impl MockProvider {
        fn new(dialect: ToolDialect, replies: Vec<Option<ProviderResponse>>) -> Self {
            MockProvider {
                base: ProviderBase::new(0, 0.0, None),
                dialect,
                replies,
                failure: Error::provider("down"),
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
            _effort: ReasoningEffort,
        ) -> Result<ProviderResponse> {
            unreachable!("the runner goes through chat_with_retries")
        }

        fn chat_with_retries(
            &self,
            _model: &str,
            messages: &[Value],
            purpose: &str,
            tools: Option<&[ToolSpec]>,
            effort: ReasoningEffort,
        ) -> Result<ProviderResponse> {
            let mut calls = self.calls.lock().expect("lock");
            calls.push(Recorded {
                messages: messages.to_vec(),
                tools: tools.map(<[ToolSpec]>::to_vec),
                purpose: purpose.to_string(),
                effort,
            });
            let index = (calls.len() - 1).min(self.replies.len() - 1);
            self.replies[index]
                .clone()
                .ok_or_else(|| self.failure.clone())
        }
    }

    fn step_to_completion(
        runner: &mut AgentRunner<'_>,
        dispatcher: &ToolDispatcher,
        reason: TurnReason,
    ) -> Result<String> {
        let mut turn = runner.start(&[], "turn", None);
        for _ in 0..100 {
            if let Some(text) = turn.text() {
                assert_eq!(turn.reason(), Some(reason));
                return Ok(text.to_string());
            }
            if let Some(call) = turn.pending_call().cloned() {
                turn.accept_tool_result(dispatcher.execute(&call, "Alice"))?;
            } else {
                runner.advance(&mut turn)?;
            }
            turn = AgentTurn::from_snapshot(&turn.snapshot())?;
        }
        Err(Error::session(
            "The scripted turn did not complete in 100 steps",
        ))
    }

    #[test]
    fn a_reply_without_tool_calls_is_returned_as_is() {
        let provider = MockProvider::text(&["My view."]);
        let (agent, dispatcher) = fixture(Vec::new());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), "My view.");
    }

    /// The level is configured on the agent and read on every call the agent
    /// makes, follow-ups included — a turn that thinks hard until it uses a
    /// tool and then coasts would be the wrong shape.
    #[test]
    fn the_agents_own_effort_rides_along_on_every_call_of_the_turn() {
        let provider = MockProvider::text(&[call_block("ping").as_str(), "Got pong."]);
        let (mut agent, dispatcher) = fixture(ping());
        agent.reasoning_effort = Some(ReasoningEffort::Low);
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        runner.run(&[], "turn", None).expect("a turn");

        assert_eq!(provider.call_count(), 2);
        assert_eq!(provider.call(0).effort, ReasoningEffort::Low);
        assert_eq!(provider.call(1).effort, ReasoningEffort::Low);
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
            step_to_completion(&mut runner, &dispatcher, TurnReason::Completed).expect("a turn"),
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
            step_to_completion(&mut runner, &dispatcher, TurnReason::InvalidCalls).expect("a turn"),
            "```tool_calls\n{bad\n```"
        );
        // Each bad response costs one call; the third trips the bound.
        assert_eq!(provider.call_count(), MAX_INVALID_CALLS as usize);
    }

    #[test]
    fn a_model_repeating_one_failing_call_does_not_loop_forever() {
        // The block parses, so MAX_INVALID_CALLS never sees it, and the tool
        // result is an error the model declines to act on. Nothing else in the
        // turn is watching for the model making no progress.
        let block = call_block("teleport");
        let provider = MockProvider::text(&[block.as_str()]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            step_to_completion(&mut runner, &dispatcher, TurnReason::RepeatedFailures)
                .expect("a turn"),
            block
        );
        // The opening call, then one per repeat until the bound trips.
        assert_eq!(
            provider.call_count(),
            MAX_REPEATED_FAILURES as usize + 1,
            "{:?}",
            provider.call_count()
        );
    }

    #[test]
    fn a_failing_call_the_model_varies_is_left_alone() {
        // Only an identical round is no progress. A model working through
        // different wrong calls is still working, and the guard has to let it.
        let provider = MockProvider::text(&[
            call_block("teleport").as_str(),
            call_block("levitate").as_str(),
            call_block("teleport").as_str(),
            call_block("levitate").as_str(),
            "I give up, here is my answer.",
        ]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");

        assert_eq!(
            runner.run(&[], "turn", None).expect("a turn"),
            "I give up, here is my answer."
        );
    }

    #[test]
    fn a_tool_that_keeps_succeeding_is_never_cut_off() {
        // Every result carries new information as far as the turn can tell, so
        // repeating a *working* call is the model's business, not the guard's.
        let block = call_block("ping");
        let provider = MockProvider::text(&[block.as_str()]);
        let (agent, dispatcher) = fixture(ping());
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE")
            .with_max_tool_iterations(MAX_REPEATED_FAILURES + 4);

        assert_eq!(runner.run(&[], "turn", None).expect("a turn"), block);
        // Only the iteration bound stopped it.
        assert_eq!(
            provider.call_count(),
            MAX_REPEATED_FAILURES as usize + 5,
            "the repeat guard fired on a succeeding tool"
        );
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

        assert_eq!(
            step_to_completion(&mut runner, &dispatcher, TurnReason::ToolIterations)
                .expect("a turn"),
            block
        );
        // 1 opening call + 2 followups, then the bound stops it.
        assert_eq!(provider.call_count(), 3);

        let mut turn = runner.start(&[], "turn", None);
        runner.advance(&mut turn).expect("pending tool");
        let pending = turn.snapshot();
        for (field, value) in [
            ("iterations", u32::MAX),
            ("invalid", u32::MAX),
            ("repeated", u32::MAX),
            ("invalid", MAX_INVALID_CALLS),
            ("repeated", MAX_REPEATED_FAILURES),
        ] {
            let mut corrupt = pending.clone();
            corrupt[field] = json!(value);
            assert!(AgentTurn::from_snapshot(&corrupt).is_err(), "{field}");
            // The public serde implementation is also a restoration path.
            let mut unchecked: AgentTurn = serde_json::from_value(corrupt).unwrap();
            assert!(
                unchecked
                    .accept_tool_result(ToolResult {
                        name: "ping".into(),
                        content: "pong".into(),
                        is_error: false,
                    })
                    .is_err(),
                "{field}"
            );
            assert!(runner.advance(&mut unchecked).is_err(), "{field}");
        }
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
        let provider = MockProvider::text(&[call_block("ping").as_str(), "done"]);
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");
        let mut turn = runner.start(&[], "turn", None);
        runner.advance(&mut turn).expect("opening");
        assert_eq!(turn.take_recorded().len(), 1);
        turn = AgentTurn::from_snapshot(&turn.snapshot()).expect("restore drained outbox");
        assert!(turn.take_recorded().is_empty());
        let call = turn.pending_call().cloned().expect("ping");
        turn.accept_tool_result(dispatcher.execute(&call, "Alice"))
            .expect("result");
        assert_eq!(turn.take_recorded().len(), 2);
        assert!(turn.take_recorded().is_empty());
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
        let mut turn = runner.start(&[], "turn from Alice", None);
        runner
            .advance(&mut turn)
            .expect("legacy failure is absorbed");
        assert_eq!(turn.failure(), Some("down"));
        assert_eq!(turn.reason(), Some(TurnReason::ProviderFailure));
        let mut runner = runner.with_strict_errors();
        let mut turn = runner.start(&[], "turn from Alice", None);
        let before = turn.snapshot();
        assert!(runner
            .advance(&mut turn)
            .expect_err("strict failure")
            .is_provider());
        assert_eq!(
            turn.snapshot(),
            before,
            "failure leaves a retryable continuation"
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
        let mut provider = MockProvider::new(
            ToolDialect::Text,
            vec![
                Some(ProviderResponse::text(call_block("ping"))),
                None,
                Some(ProviderResponse::text("done after compaction")),
            ],
        );
        provider.failure = Error::ProviderHttp {
            status_code: 413,
            url: "https://provider.test".to_string(),
            body: "maximum context length".to_string(),
        };
        let mut runner = AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE");
        let mut turn = runner.start(
            &[json!({"role":"user", "content":"old history"})],
            "turn",
            Some("keep instruction"),
        );
        runner.advance(&mut turn).expect("opening tool call");
        let call = turn.pending_call().cloned().expect("ping");
        turn.accept_tool_result(dispatcher.execute(&call, "Alice"))
            .expect("tool result");
        let before = turn.snapshot();
        assert!(runner
            .advance(&mut turn)
            .expect_err("overflow reaches the session")
            .is_context_overflow());
        assert_eq!(turn.snapshot(), before);
        turn.replace_history(&[])
            .expect("replace only shared history");
        turn = AgentTurn::from_snapshot(&turn.snapshot()).expect("resume after compaction");
        assert!(
            turn.pending_call().is_none(),
            "the completed tool must not be replayed"
        );
        runner
            .advance(&mut turn)
            .expect("retry with compacted history");
        assert_eq!(turn.text(), Some("done after compaction"));
        let sent = contents(&provider.call(2).messages);
        assert!(sent.iter().any(|text| text == "keep instruction"));
        assert_eq!(
            sent.iter()
                .filter(|text| text.as_str() == "[Tool:ping] pong")
                .count(),
            1
        );
        assert!(!sent.iter().any(|text| text == "old history"));
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
                    tool_calls: vec![
                        ToolCall::new("ping", Arguments::new()).with_id("c1"),
                        ToolCall::new("ping", Arguments::new()).with_id("c2"),
                    ],
                    stop_reason: "tool_calls".to_string(),
                    ..ProviderResponse::default()
                }),
                Some(ProviderResponse::text("Got pong.")),
            ],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let tool = ToolSpec::new(
            "ping",
            "ping tool",
            json!({"type": "object", "properties": {}}),
            Arc::new(move |_: &Arguments, _: &str| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok("pong".to_string())
            }),
        );
        let (agent, dispatcher) = fixture(vec![tool]);
        let mut runner =
            AgentRunner::new(&agent, &provider, messages_for, &dispatcher, "BASE").with_tools(ping);

        let mut turn = runner.start(&[], "turn", None);
        runner.advance(&mut turn).expect("provider reply");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "advance executes no tools");
        for id in ["c1", "c2"] {
            let call = turn.pending_call().cloned().expect("a pending call");
            assert_eq!(call.id, id);
            assert!(
                runner.advance(&mut turn).is_err(),
                "an incomplete native batch cannot reach the provider"
            );
            assert_eq!(provider.call_count(), 1);
            turn.accept_tool_result(dispatcher.execute(&call, "Alice"))
                .expect("accept result");
            turn = AgentTurn::from_snapshot(&turn.snapshot()).expect("restore between calls");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        runner.advance(&mut turn).expect("followup");
        assert_eq!(turn.text(), Some("Got pong."));
        assert_eq!(turn.reason(), Some(TurnReason::Completed));
        assert_eq!(provider.call_count(), 2);

        let followup = provider.call(1).messages;
        let [.., assistant, first_result, result] = followup.as_slice() else {
            panic!("the exchange is replayed: {followup:?}");
        };
        assert_eq!(assistant["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(assistant["tool_calls"][1]["id"], json!("c2"));
        assert_eq!(first_result["tool_call_id"], json!("c1"));
        assert_eq!(
            *result,
            json!({"role": "tool", "tool_call_id": "c2", "content": "pong"})
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
