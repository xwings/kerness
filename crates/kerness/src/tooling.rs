//! Tool definitions and the text-fence call protocol.
//!
//! Providers without native tool calling get this instead: the model writes a
//! fenced JSON block, and [`parse_tool_calls`] reads it. The parser is a
//! recovery layer rather than a strict wire decoder — a mislabeled fence or a
//! bare object still counts — because a malformed batch that vanishes costs a
//! turn, while one that comes back as an error costs a tool call.

use std::fmt;
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::Result;
use crate::pyfmt::truthy;

/// The parsed arguments of a tool call.
pub type Arguments = Map<String, Value>;

/// The name a call carries when it could not be parsed.
///
/// A reserved name rather than a flag: the dispatcher already turns unknown
/// names into readable results, so an unparseable batch travels the same path
/// as every other thing the model got wrong.
pub const INVALID_CALL: &str = "__invalid_tool_calls__";

/// What a tool actually does when called.
///
/// The result is text because that is what reaches the model. Handlers that
/// compute something else render it themselves.
pub trait ToolHandler: Send + Sync {
    fn call(&self, arguments: &Arguments, actor: &str) -> Result<String>;
}

impl<F> ToolHandler for F
where
    F: Fn(&Arguments, &str) -> Result<String> + Send + Sync,
{
    fn call(&self, arguments: &Arguments, actor: &str) -> Result<String> {
        self(arguments, actor)
    }
}

/// A tool an agent may call.
#[derive(Clone)]
pub struct ToolSpec {
    /// Identifier the model uses.
    pub name: String,
    /// What it does, shown to the model.
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: Value,
    /// Called with the parsed arguments.
    pub handler: Arc<dyn ToolHandler>,
    /// Whether the handler needs to know which agent is calling.
    ///
    /// The built-in `cmd`/`read_file`/`list_dir` tools set this because
    /// access-control prompts and the command log name the actor; tools
    /// registered through `Session::add_tool` do not.
    pub takes_actor: bool,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: Arc<dyn ToolHandler>,
    ) -> Self {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters,
            handler,
            takes_actor: false,
        }
    }

    /// Mark the handler as wanting the calling agent's name.
    pub fn with_actor(mut self) -> Self {
        self.takes_actor = true;
        self
    }
}

impl PartialEq for ToolSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.description == other.description
            && self.parameters == other.parameters
            && self.takes_actor == other.takes_actor
            && Arc::ptr_eq(&self.handler, &other.handler)
    }
}

impl fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolSpec")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("takes_actor", &self.takes_actor)
            .finish_non_exhaustive()
    }
}

/// A call the model asked for.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ToolCall {
    /// The tool to run.
    pub name: String,
    /// Parsed arguments, validated at dispatch.
    pub arguments: Arguments,
    /// Provider-assigned correlation id.
    ///
    /// Both native APIs require the result to name the call it answers; under
    /// TEXT nothing correlates on it, but the fence format carries one so it
    /// is preserved.
    pub id: String,
}

impl ToolCall {
    pub fn new(name: impl Into<String>, arguments: Arguments) -> Self {
        ToolCall {
            name: name.into(),
            arguments,
            id: String::new(),
        }
    }

    /// A call standing in for a batch that could not be read.
    pub fn invalid(error: impl Into<String>) -> Self {
        let mut arguments = Arguments::new();
        arguments.insert("error".into(), Value::String(error.into()));
        ToolCall::new(INVALID_CALL, arguments)
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"```([a-zA-Z0-9_-]+)\s*$").expect("static pattern"));

/// Sentinel for a fence that opened and never closed, kept distinct from "no
/// fence at all" because the two mean opposite things to the model.
const UNCLOSED: &str = "__UNCLOSED_FENCE__";

/// Parse OpenAI function-style tool calls from a fenced JSON block.
///
/// A ```` ```json ```` fence is accepted as well as ```` ```tool_calls ````,
/// because models routinely mislabel the fence. But it counts only when the
/// payload actually mentions `tool_calls` — the same test the bare-object path
/// applies. Without that, the ```` ```json ```` result block a gameplan's
/// `result:` shape asks for parses as a malformed call, comes back as an error
/// result, and the model is asked again for the same closing summary
/// indefinitely.
pub fn parse_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut raw = extract_fenced_json(text, &["tool_calls"]);
    if raw.is_empty() {
        let loose = extract_fenced_json(text, &["json"]);
        if contains_tool_calls_key(&loose) {
            raw = loose;
        }
    }
    if raw == UNCLOSED {
        return vec![ToolCall::invalid("unclosed tool_calls fence")];
    }
    if raw.is_empty() {
        let candidate = text.trim();
        if candidate.starts_with('{') && contains_tool_calls_key(candidate) {
            raw = candidate.to_string();
        } else {
            return Vec::new();
        }
    }

    let payload: Value = match serde_json::from_str(&raw) {
        Ok(payload) => payload,
        Err(err) => return vec![ToolCall::invalid(err.to_string())],
    };

    let calls: &[Value] = match &payload {
        Value::Object(map) if map.contains_key("tool_calls") => {
            let calls = map.get("tool_calls").expect("key just checked");
            if !truthy(calls) {
                return vec![ToolCall::invalid("empty tool_calls")];
            }
            // A truthy non-list holds nothing callable; it falls through to
            // the "no valid calls" report rather than to "empty".
            calls.as_array().map_or(&[][..], Vec::as_slice)
        }
        Value::Array(items) => items.as_slice(),
        _ => return vec![ToolCall::invalid("missing tool_calls")],
    };

    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(call) = call.as_object() else {
            continue;
        };
        let function = match call.get("function") {
            Some(Value::Object(function)) => Some(function),
            _ => None,
        };
        let name = function
            .and_then(|f| f.get("name"))
            .filter(|name| truthy(name))
            .or_else(|| call.get("name"));
        let Some(name) = name.filter(|name| truthy(name)).map(crate::pyfmt::str) else {
            continue;
        };
        let raw_arguments = function
            .and_then(|f| f.get("arguments"))
            .filter(|arguments| truthy(arguments))
            .or_else(|| call.get("arguments"));
        results.push(ToolCall {
            name,
            arguments: decode_arguments(raw_arguments),
            id: call
                .get("id")
                .filter(|id| truthy(id))
                .map(crate::pyfmt::str)
                .unwrap_or_default(),
        });
    }

    if results.is_empty() {
        return vec![ToolCall::invalid("tool_calls contains no valid calls")];
    }
    results
}

/// Arguments arrive as an object, as a JSON string holding one, or as
/// something else entirely. Anything that is not an object is preserved under
/// `raw` so the model can see what it sent.
fn decode_arguments(raw: Option<&Value>) -> Arguments {
    let raw = match raw {
        None => return Arguments::new(),
        Some(value) => value,
    };
    let decoded = match raw {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => parsed,
            Err(_) => return wrap_raw(raw.clone()),
        },
        other => other.clone(),
    };
    match decoded {
        Value::Object(map) => map,
        other => wrap_raw(other),
    }
}

fn wrap_raw(value: Value) -> Arguments {
    let mut arguments = Arguments::new();
    arguments.insert("raw".into(), value);
    arguments
}

/// Whether JSON text declares the exact `tool_calls` object key.
///
/// Hand-scanned rather than matched, because the pattern needs a lookbehind to
/// reject `my_tool_calls:` and the `regex` crate has none.
fn contains_tool_calls_key(text: &str) -> bool {
    const KEY: &str = "tool_calls";
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(KEY) {
        let start = from + offset;
        let end = start + KEY.len();
        let quoted = start > 0 && bytes[start - 1] == b'"' && bytes.get(end) == Some(&b'"');
        let (token_start, after) = if quoted {
            (start - 1, end + 1)
        } else {
            (start, end)
        };
        let preceded_ok = token_start == 0 || !is_token_byte(bytes[token_start - 1]);
        if preceded_ok && followed_by_colon(bytes, after) {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn followed_by_colon(bytes: &[u8], mut index: usize) -> bool {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    bytes.get(index) == Some(&b':')
}

/// Extract the first fenced block whose label is one of *fence_names*.
///
/// Returns the block body, `""` when no such fence opened, or [`UNCLOSED`]
/// when one opened and the text ended first.
fn extract_fenced_json(text: &str, fence_names: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if let Some(captures) = FENCE_RE.captures(line) {
            let fence = captures[1].to_ascii_lowercase();
            if fence_names.contains(&fence.as_str()) {
                index += 1;
                let mut body: Vec<&str> = Vec::new();
                while index < lines.len() {
                    if lines[index].trim() == "```" {
                        return body.join("\n").trim().to_string();
                    }
                    body.push(lines[index]);
                    index += 1;
                }
                return UNCLOSED.to_string();
            }
        }
        index += 1;
    }
    String::new()
}

/// Format available tools and the text-protocol instructions.
pub fn format_tools_prompt(tools: &[ToolSpec]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "Available tools (OpenAI function-style):".into(),
        "- If you need a tool, respond with ONLY a ```tool_calls``` fenced block.".into(),
        "- Otherwise, respond normally in plain text (no tool_calls).".into(),
        "- Do NOT use ```json``` fences and do NOT output raw JSON.".into(),
        "- Do NOT include any extra text outside the tool_calls block when calling tools.".into(),
        "- cmd runs a single command only. Do NOT use shell operators like &&, |, ;, ||, or redirections.".into(),
        "- Do NOT use tools for normal text replies. If you can answer in text, answer directly without any tool call.".into(),
        r#"- Do NOT output empty tool_calls (e.g., {"tool_calls": []})."#.into(),
        r#"- Format: {"tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "tool_name", "arguments": "{...json...}"}}]}"#.into(),
        String::new(),
        "Tool definitions:".into(),
    ];
    for tool in tools {
        lines.push(format!("- {}: {}", tool.name, tool.description));
        lines.push(format!(
            "  parameters: {}",
            crate::pyfmt::json_dumps(&tool.parameters)
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arguments(value: Value) -> Arguments {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn a_tool_calls_fence_is_parsed() {
        let text = concat!(
            "```tool_calls\n",
            r#"{"tool_calls": [{"id": "call_1", "function": {"name": "cmd", "arguments": "{\"command\": \"ls\"}"}}]}"#,
            "\n```"
        );
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "cmd");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].arguments, arguments(json!({"command": "ls"})));
    }

    #[test]
    fn a_mislabeled_json_fence_counts_only_when_it_mentions_tool_calls() {
        let calling = "```json\n{\"tool_calls\": [{\"name\": \"ping\"}]}\n```";
        assert_eq!(parse_tool_calls(calling)[0].name, "ping");

        let result_block = "```json\n{\"summary\": \"done\"}\n```";
        assert!(
            parse_tool_calls(result_block).is_empty(),
            "a result block is not a tool call"
        );
    }

    #[test]
    fn a_bare_object_counts_only_with_the_exact_key() {
        assert_eq!(
            parse_tool_calls(r#"{"tool_calls": [{"name": "ping"}]}"#)[0].name,
            "ping"
        );
        assert!(parse_tool_calls(r#"{"my_tool_calls": [{"name": "ping"}]}"#).is_empty());
    }

    #[test]
    fn malformed_batches_come_back_as_readable_errors() {
        let unclosed = parse_tool_calls("```tool_calls\n{\"tool_calls\": []}");
        assert_eq!(unclosed[0].name, INVALID_CALL);
        assert_eq!(
            unclosed[0].arguments["error"],
            json!("unclosed tool_calls fence")
        );

        let empty = parse_tool_calls("```tool_calls\n{\"tool_calls\": []}\n```");
        assert_eq!(empty[0].arguments["error"], json!("empty tool_calls"));

        let missing = parse_tool_calls("```tool_calls\n\"nope\"\n```");
        assert_eq!(missing[0].arguments["error"], json!("missing tool_calls"));

        let nameless = parse_tool_calls("```tool_calls\n{\"tool_calls\": [{\"id\": \"1\"}]}\n```");
        assert_eq!(
            nameless[0].arguments["error"],
            json!("tool_calls contains no valid calls")
        );
    }

    #[test]
    fn unparseable_arguments_are_preserved_under_raw() {
        let text = "```tool_calls\n{\"tool_calls\": [{\"name\": \"cmd\", \"arguments\": \"not json\"}]}\n```";
        let calls = parse_tool_calls(text);
        assert_eq!(calls[0].arguments, arguments(json!({"raw": "not json"})));
    }

    #[test]
    fn an_empty_toolkit_produces_no_prompt_block() {
        assert_eq!(format_tools_prompt(&[]), "");
    }
}
