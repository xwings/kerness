//! Per-API tool schema conversion, call parsing, and message rendering.
//!
//! The two native APIs disagree about every part of tool calling: where the
//! schema goes, what the schema key is called, how a call comes back, and how a
//! result is fed in. Rather than pick a lowest common denominator, this module
//! holds one converter per dialect and everything above it stays
//! dialect-neutral.
//!
//! The differences, concretely:
//!
//! | | OpenAI | Anthropic |
//! | --- | --- | --- |
//! | schema | nested under `function` | flat |
//! | schema key | `parameters` | `input_schema` |
//! | call arrives as | `message.tool_calls[]` | a `tool_use` content block |
//! | arguments are | a JSON **string** | a JSON **object** |
//! | result goes in | a `role: "tool"` message | a `tool_result` block in a `role: "user"` message |
//!
//! [`ToolDialect::Text`] is the fallback for endpoints with no native support.
//! It is not a lesser sibling here — it is the only dialect every provider can
//! speak, and its rendering is byte-for-byte what the session produced before
//! native calling existed.

use serde_json::{json, Value};

use crate::provider::ProviderResponse;
use crate::pyfmt::json_dumps;
use crate::tooling::{Arguments, ToolCall, ToolSpec};
use crate::toolkit::ToolResult;

/// How a provider expects tool schemas and tool results to be encoded.
///
/// `Text` is the fallback: tools are described in prose and calls are scraped
/// out of a fenced JSON block. `Openai` and `Anthropic` send real schemas in
/// the request and read structured tool-use blocks back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolDialect {
    #[default]
    Text,
    Openai,
    Anthropic,
}

impl ToolDialect {
    /// The wire name, which is also what a gameplan writes.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolDialect::Text => "text",
            ToolDialect::Openai => "openai",
            ToolDialect::Anthropic => "anthropic",
        }
    }

    /// Parse a wire name, or `None` when it names no dialect.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(ToolDialect::Text),
            "openai" => Some(ToolDialect::Openai),
            "anthropic" => Some(ToolDialect::Anthropic),
            _ => None,
        }
    }
}

impl std::fmt::Display for ToolDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convert a spec to the OpenAI function-tool wire format.
pub fn to_openai_tool(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "parameters": spec.parameters,
        },
    })
}

/// Convert a spec to the Anthropic tool wire format.
pub fn to_anthropic_tool(spec: &ToolSpec) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "input_schema": spec.parameters,
    })
}

/// Convert specs for *dialect*, or `None` when there is nothing to send.
///
/// `None` for the TEXT dialect and for an empty tool set — in both cases the
/// request carries no `tools` key at all.
pub fn tool_schemas(dialect: ToolDialect, tools: &[ToolSpec]) -> Option<Vec<Value>> {
    if tools.is_empty() || dialect == ToolDialect::Text {
        return None;
    }
    Some(match dialect {
        ToolDialect::Anthropic => tools.iter().map(to_anthropic_tool).collect(),
        _ => tools.iter().map(to_openai_tool).collect(),
    })
}

/// Extract tool calls from an OpenAI `choices[].message`.
///
/// Arguments arrive as a JSON string. A malformed one is kept verbatim under
/// `raw` rather than dropped, so the dispatcher reports a schema error the
/// model can correct instead of the call vanishing.
pub fn parse_openai_tool_calls(message: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for raw in array_at(message, "tool_calls") {
        let Some(raw) = raw.as_object() else { continue };
        let function = match raw.get("function") {
            Some(Value::Object(function)) => Some(function),
            _ => None,
        };
        let Some(name) = function
            .and_then(|function| function.get("name"))
            .filter(|name| crate::pyfmt::truthy(name))
            .map(crate::pyfmt::str)
        else {
            continue;
        };
        calls.push(ToolCall {
            name,
            arguments: decode_arguments(function.and_then(|function| function.get("arguments"))),
            id: identifier(raw.get("id")),
        });
    }
    calls
}

/// Extract tool calls from an Anthropic messages response.
pub fn parse_anthropic_tool_calls(response: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    for block in array_at(response, "content") {
        let Some(block) = block.as_object() else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = block
            .get("name")
            .filter(|name| crate::pyfmt::truthy(name))
            .map(crate::pyfmt::str)
        else {
            continue;
        };
        let arguments = match block.get("input") {
            Some(Value::Object(input)) => input.clone(),
            other => {
                let mut wrapped = Arguments::new();
                wrapped.insert("raw".into(), other.cloned().unwrap_or(Value::Null));
                wrapped
            }
        };
        calls.push(ToolCall {
            name,
            arguments,
            id: identifier(block.get("id")),
        });
    }
    calls
}

/// The array under *key*, or nothing when it is absent or another type.
fn array_at<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

/// A correlation id, coerced to text the way `str(raw.get("id") or "")` does.
fn identifier(raw: Option<&Value>) -> String {
    raw.filter(|id| crate::pyfmt::truthy(id))
        .map(crate::pyfmt::str)
        .unwrap_or_default()
}

/// Arguments as an object, as a JSON string holding one, or as neither.
///
/// Distinct from the text-fence decoder: a native call that sends something
/// other than an object or a string sent no arguments at all, whereas the
/// fence protocol preserves whatever the model typed.
fn decode_arguments(raw: Option<&Value>) -> Arguments {
    match raw {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::String(text)) => {
            let source = if text.is_empty() { "{}" } else { text.as_str() };
            match serde_json::from_str::<Value>(source) {
                Ok(Value::Object(map)) => map,
                Ok(other) => wrap_raw(other),
                Err(_) => wrap_raw(Value::String(text.clone())),
            }
        }
        _ => Arguments::new(),
    }
}

fn wrap_raw(value: Value) -> Arguments {
    let mut arguments = Arguments::new();
    arguments.insert("raw".into(), value);
    arguments
}

/// Render the model's tool-calling turn back into the message list.
///
/// Both native APIs require the assistant turn that made the calls to be
/// replayed before their results; omitting it is a 400.
pub fn render_assistant_turn(dialect: ToolDialect, response: &ProviderResponse) -> Value {
    match dialect {
        ToolDialect::Openai => json!({
            "role": "assistant",
            "content": if response.content.is_empty() {
                Value::Null
            } else {
                Value::String(response.content.clone())
            },
            "tool_calls": response
                .tool_calls
                .iter()
                .map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": json_dumps(&Value::Object(call.arguments.clone())),
                    },
                }))
                .collect::<Vec<_>>(),
        }),
        ToolDialect::Anthropic => {
            let mut blocks: Vec<Value> = Vec::with_capacity(response.tool_calls.len() + 1);
            if !response.content.is_empty() {
                blocks.push(json!({"type": "text", "text": response.content}));
            }
            blocks.extend(response.tool_calls.iter().map(|call| {
                json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": Value::Object(call.arguments.clone()),
                })
            }));
            json!({"role": "assistant", "content": blocks})
        }
        ToolDialect::Text => json!({"role": "assistant", "content": response.content}),
    }
}

/// Render one tool result as the message the model reads next.
pub fn render_tool_result(dialect: ToolDialect, call: &ToolCall, result: &ToolResult) -> Value {
    match dialect {
        ToolDialect::Openai => json!({
            "role": "tool",
            "tool_call_id": call.id,
            "content": error_text(result),
        }),
        ToolDialect::Anthropic => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": call.id,
                "content": result.content,
                "is_error": result.is_error,
            }],
        }),
        ToolDialect::Text => json!({
            "role": "assistant",
            "content": format!("[Tool:{}] {}", result.name, error_text(result)),
        }),
    }
}

/// Mark failures inline for dialects with no error flag of their own.
fn error_text(result: &ToolResult) -> String {
    if result.is_error {
        format!("[ToolError] {}", result.content)
    } else {
        result.content.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn spec() -> ToolSpec {
        ToolSpec::new(
            "cmd",
            "Run a command",
            json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            Arc::new(|_: &Arguments, _: &str| Ok(String::new())),
        )
    }

    #[test]
    fn each_dialect_puts_the_schema_where_its_api_expects_it() {
        let tools = [spec()];
        assert_eq!(
            tool_schemas(ToolDialect::Openai, &tools).expect("schemas")[0]["function"]["parameters"],
            spec().parameters
        );
        assert_eq!(
            tool_schemas(ToolDialect::Anthropic, &tools).expect("schemas")[0]["input_schema"],
            spec().parameters
        );
    }

    #[test]
    fn a_request_with_no_tools_to_send_carries_no_tools_key() {
        assert!(tool_schemas(ToolDialect::Text, &[spec()]).is_none());
        assert!(tool_schemas(ToolDialect::Openai, &[]).is_none());
    }

    #[test]
    fn openai_arguments_arrive_as_a_json_string() {
        let message = json!({
            "tool_calls": [{
                "id": "call_1",
                "function": {"name": "cmd", "arguments": "{\"command\": \"ls\"}"},
            }],
        });
        let calls = parse_openai_tool_calls(&message);
        assert_eq!(calls[0].name, "cmd");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].arguments["command"], json!("ls"));
    }

    #[test]
    fn malformed_openai_arguments_are_preserved_rather_than_dropped() {
        let message = json!({
            "tool_calls": [{"function": {"name": "cmd", "arguments": "not json"}}],
        });
        let calls = parse_openai_tool_calls(&message);
        assert_eq!(calls[0].arguments["raw"], json!("not json"));
        assert_eq!(calls[0].id, "", "an id the API omitted stays empty");
    }

    #[test]
    fn anthropic_arguments_arrive_as_an_object_and_other_blocks_are_ignored() {
        let response = json!({
            "content": [
                {"type": "text", "text": "thinking"},
                {"type": "tool_use", "id": "tu_1", "name": "cmd", "input": {"command": "ls"}},
            ],
        });
        let calls = parse_anthropic_tool_calls(&response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "tu_1");
        assert_eq!(calls[0].arguments["command"], json!("ls"));
    }

    #[test]
    fn the_assistant_turn_is_replayed_in_each_apis_own_shape() {
        let response = ProviderResponse {
            content: "checking".into(),
            tool_calls: vec![ToolCall::new(
                "cmd",
                json!({"command": "ls"}).as_object().cloned().expect("object"),
            )
            .with_id("call_1")],
            ..ProviderResponse::default()
        };

        let openai = render_assistant_turn(ToolDialect::Openai, &response);
        assert_eq!(
            openai["tool_calls"][0]["function"]["arguments"],
            json!("{\"command\": \"ls\"}"),
            "OpenAI wants the arguments back as a string"
        );

        let anthropic = render_assistant_turn(ToolDialect::Anthropic, &response);
        assert_eq!(anthropic["content"][0]["type"], json!("text"));
        assert_eq!(anthropic["content"][1]["input"], json!({"command": "ls"}));

        let text = render_assistant_turn(ToolDialect::Text, &response);
        assert_eq!(text, json!({"role": "assistant", "content": "checking"}));
    }

    #[test]
    fn a_content_free_tool_turn_sends_null_rather_than_an_empty_string() {
        let response = ProviderResponse {
            tool_calls: vec![ToolCall::new("cmd", Arguments::new())],
            ..ProviderResponse::default()
        };
        assert_eq!(
            render_assistant_turn(ToolDialect::Openai, &response)["content"],
            Value::Null
        );
        assert!(
            render_assistant_turn(ToolDialect::Anthropic, &response)["content"]
                .as_array()
                .expect("blocks")
                .iter()
                .all(|block| block["type"] != json!("text")),
            "an empty text block is a 400"
        );
    }

    #[test]
    fn only_the_dialects_without_an_error_flag_mark_failures_inline() {
        let call = ToolCall::new("cmd", Arguments::new()).with_id("call_1");
        let failed = ToolResult {
            name: "cmd".into(),
            content: "denied".into(),
            is_error: true,
        };

        assert_eq!(
            render_tool_result(ToolDialect::Openai, &call, &failed)["content"],
            json!("[ToolError] denied")
        );
        assert_eq!(
            render_tool_result(ToolDialect::Text, &call, &failed)["content"],
            json!("[Tool:cmd] [ToolError] denied")
        );

        let anthropic = render_tool_result(ToolDialect::Anthropic, &call, &failed);
        assert_eq!(anthropic["content"][0]["is_error"], json!(true));
        assert_eq!(
            anthropic["content"][0]["content"],
            json!("denied"),
            "Anthropic has its own flag, so the text stays clean"
        );
    }
}
