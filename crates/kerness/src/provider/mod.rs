//! LLM backends and the shape of what they answer.

use serde_json::{Map, Value};

use crate::tooling::ToolCall;

/// One reply from a provider, normalized across backends.
///
/// Every field above `content` is optional detail a caller may want and the
/// framework mostly does not read: `raw` is the untouched response body,
/// `usage` is whatever token counts the backend reported, and `structured`
/// holds the decoded JSON when the caller asked for structured output. Keeping
/// them means a caller never has to re-issue a request to see something the
/// provider already sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderResponse {
    /// The text the model produced. Empty on a pure tool-calling turn.
    pub content: String,
    /// The model that answered, as the backend named it.
    pub model: String,
    /// Token counts as reported, un-normalized — backends disagree on the key
    /// names and inventing a common set would mean discarding what was sent.
    pub usage: Map<String, Value>,
    /// The response body, untouched.
    pub raw: Value,
    /// The decoded JSON body when structured output was requested.
    pub structured: Option<Value>,
    /// Tool calls the model made, in the order it made them.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped, as the backend named it.
    pub stop_reason: String,
}

impl ProviderResponse {
    /// A plain text reply, with no tool calls and no backend detail.
    pub fn text(content: impl Into<String>) -> Self {
        ProviderResponse {
            content: content.into(),
            ..ProviderResponse::default()
        }
    }
}
