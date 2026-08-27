//! Tool dispatch: what an agent may call, and what comes back when it does.
//!
//! Dispatch **never fails**. Every failure — unknown tool, bad arguments, a
//! denied command, a handler blowing up — becomes a [`ToolResult`] with
//! `is_error` set, which the runtime feeds back to the model as its chance to
//! correct itself. That is the standard agentic contract, and it keeps
//! re-prompt logic out of the session.
//!
//! The handler is the only path: `cmd`, `read_file`, and `list_dir` get no
//! branches beside their registered handlers, and argument checking is
//! [`validate_arguments`], which reads the same schema the model was shown.

use std::sync::Arc;

use crate::jsonschema::validate_arguments;
use crate::tooling::{ToolCall, ToolSpec, INVALID_CALL};

/// The outcome of one tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    /// The tool that was called.
    pub name: String,
    /// Rendered output, or the error text when `is_error`.
    pub content: String,
    /// True when the call did not produce a usable result.
    pub is_error: bool,
}

impl ToolResult {
    fn ok(name: &str, content: String) -> Self {
        ToolResult {
            name: name.to_string(),
            content,
            is_error: false,
        }
    }

    fn error(name: &str, content: String) -> Self {
        ToolResult {
            name: name.to_string(),
            content,
            is_error: true,
        }
    }
}

/// Returns the specs currently permitted.
///
/// Read per call rather than captured once, so the harness narrowing resolved
/// during `run()` and the per-turn skill gate are both picked up.
pub type ToolsFor = Arc<dyn Fn() -> Vec<ToolSpec> + Send + Sync>;

/// Executes tool calls against the set of permitted specs.
pub struct ToolDispatcher {
    tools_for: ToolsFor,
}

impl ToolDispatcher {
    pub fn new(tools_for: ToolsFor) -> Self {
        ToolDispatcher { tools_for }
    }

    /// Run one tool call and describe what happened.
    ///
    /// *actor* is the agent making the call, passed to handlers that declare
    /// `takes_actor`. The result is readable by the model whether or not the
    /// call succeeded.
    pub fn execute(&self, call: &ToolCall, actor: &str) -> ToolResult {
        if call.name == INVALID_CALL {
            let error = call
                .arguments
                .get("error")
                .map(crate::pyfmt::str)
                .unwrap_or_else(|| "invalid tool_calls".to_string());
            return ToolResult::error(
                &call.name,
                format!(
                    "Your tool_calls JSON is invalid. Respond with ONLY a valid \
                     ```tool_calls``` fenced block. Error: {error}"
                ),
            );
        }

        let tools = (self.tools_for)();
        let Some(spec) = tools.iter().find(|tool| tool.name == call.name) else {
            return ToolResult::error(&call.name, format!("Unknown tool: {}", call.name));
        };

        let errors = validate_arguments(&spec.parameters, &call.arguments);
        if !errors.is_empty() {
            return ToolResult::error(
                &call.name,
                format!("{}: {}", call.name, errors.join("; ")),
            );
        }

        let actor = if spec.takes_actor { actor } else { "" };
        match spec.handler.call(&call.arguments, actor) {
            Ok(content) => ToolResult::ok(&call.name, content),
            Err(err) => ToolResult::error(&call.name, format!("{}: {err}", call.name)),
        }
    }
}

/// Narrow registered tools to the names a harness permits.
///
/// `None` means the harness has not been resolved yet, which is every tool; an
/// empty list means the harness asked for none. Order is registration order,
/// not allow-list order, so the prompt's tool block does not depend on how the
/// caller typed it.
pub fn resolve(tools: &[ToolSpec], allowed: Option<&[String]>) -> Vec<ToolSpec> {
    match allowed {
        None => tools.to_vec(),
        Some(allowed) => tools
            .iter()
            .filter(|tool| allowed.iter().any(|name| name == &tool.name))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::tooling::Arguments;
    use serde_json::json;

    fn spec_named(name: &str) -> ToolSpec {
        ToolSpec::new(
            name,
            format!("{name} tool"),
            json!({"type": "object", "properties": {}}),
            Arc::new(|_: &Arguments, _: &str| Ok("pong".to_string())),
        )
    }

    fn dispatcher(tools: Vec<ToolSpec>) -> ToolDispatcher {
        ToolDispatcher::new(Arc::new(move || tools.clone()))
    }

    #[test]
    fn a_successful_call_returns_the_handler_text() {
        let result = dispatcher(vec![spec_named("ping")]).execute(
            &ToolCall::new("ping", Arguments::new()),
            "",
        );
        assert_eq!(result, ToolResult::ok("ping", "pong".into()));
    }

    #[test]
    fn the_actor_reaches_only_handlers_that_asked_for_it() {
        let echo = ToolSpec::new(
            "who",
            "who tool",
            json!({"type": "object", "properties": {}}),
            Arc::new(|_: &Arguments, actor: &str| Ok(actor.to_string())),
        );
        let asking = echo.clone().with_actor();
        assert_eq!(
            dispatcher(vec![asking])
                .execute(&ToolCall::new("who", Arguments::new()), "Alice")
                .content,
            "Alice"
        );
        assert_eq!(
            dispatcher(vec![echo])
                .execute(&ToolCall::new("who", Arguments::new()), "Alice")
                .content,
            ""
        );
    }

    #[test]
    fn every_failure_is_a_result_the_model_can_read() {
        let unknown =
            dispatcher(vec![spec_named("ping")]).execute(&ToolCall::new("teleport", Arguments::new()), "");
        assert!(unknown.is_error);
        assert!(unknown.content.contains("Unknown tool: teleport"));

        let unparseable = dispatcher(vec![spec_named("ping")])
            .execute(&ToolCall::invalid("unclosed tool_calls fence"), "");
        assert!(unparseable.is_error);
        assert!(unparseable.content.contains("unclosed tool_calls fence"));

        let boom = ToolSpec::new(
            "ping",
            "ping tool",
            json!({"type": "object", "properties": {}}),
            Arc::new(|_: &Arguments, _: &str| Err(Error::session("kaboom"))),
        );
        let raised = dispatcher(vec![boom]).execute(&ToolCall::new("ping", Arguments::new()), "");
        assert!(raised.is_error);
        assert_eq!(raised.content, "ping: kaboom");
    }

    #[test]
    fn arguments_are_checked_against_the_schema() {
        let cmd = ToolSpec::new(
            "cmd",
            "cmd tool",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            }),
            Arc::new(|_: &Arguments, _: &str| Ok(String::new())),
        );
        let missing = dispatcher(vec![cmd]).execute(&ToolCall::new("cmd", Arguments::new()), "");
        assert!(missing.is_error);
        assert!(missing.content.contains("missing required argument 'command'"));
    }

    #[test]
    fn absent_is_everything_empty_is_nothing_and_order_is_registration() {
        let tools = vec![spec_named("a"), spec_named("b"), spec_named("c")];
        assert_eq!(resolve(&tools, None), tools);
        assert_eq!(resolve(&tools, Some(&[])), Vec::new());
        let picked = resolve(&tools, Some(&["c".to_string(), "a".to_string()]));
        assert_eq!(
            picked.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }
}
