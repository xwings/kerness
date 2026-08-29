//! The Anthropic Claude backend.

use serde_json::{json, Map, Value};

use super::{
    answering_model, anthropic_text, attach_tool_schemas, convert_messages_for_claude,
    reported_usage, Provider, ProviderBase, ProviderResponse, ReasoningEffort, DEFAULT_BACKOFF_SEC,
    DEFAULT_REQUEST_TIMEOUT_SEC, DEFAULT_RETRIES, DEFAULT_TEMPERATURE,
};
use crate::error::Result;
use crate::http::{self, Headers};
use crate::tooling::ToolSpec;
use crate::toolschema::{parse_anthropic_tool_calls, ToolDialect};

/// Where the API lives when the caller names no other host.
pub const CLAUDE_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The API version header value the endpoint requires.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The reply ceiling sent when the caller names none.
///
/// Anthropic requires the field, so there is no "unset" to send; the bindings
/// declare the same number as a Python default, which is why it is named here
/// rather than written twice.
pub const DEFAULT_CLAUDE_MAX_TOKENS: i64 = 4096;

/// How the request proves who is asking.
///
/// The two forms are mutually exclusive headers rather than two backends: the
/// request they build is otherwise identical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeCredential {
    ApiKey(String),
    OAuth(String),
}

impl ClaudeCredential {
    fn headers(&self) -> Headers {
        let credential = match self {
            ClaudeCredential::ApiKey(key) => ("x-api-key".to_string(), key.clone()),
            ClaudeCredential::OAuth(token) => {
                ("Authorization".to_string(), format!("Bearer {token}"))
            }
        };
        vec![
            credential,
            (
                "anthropic-version".to_string(),
                ANTHROPIC_VERSION.to_string(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }
}

/// Everything [`ClaudeProvider`] is built from.
pub struct ClaudeConfig {
    pub credential: ClaudeCredential,
    pub base_url: String,
    pub timeout_sec: u64,
    /// Extra attempts after the first, so `0` still calls once.
    pub retries: u32,
    pub backoff_sec: f64,
    pub interval_sec: Option<f64>,
    pub temperature: f64,
    /// Required by the endpoint, so it has a value rather than being optional.
    pub max_tokens: i64,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        ClaudeConfig {
            credential: ClaudeCredential::ApiKey(String::new()),
            base_url: CLAUDE_BASE_URL.to_string(),
            timeout_sec: DEFAULT_REQUEST_TIMEOUT_SEC,
            retries: DEFAULT_RETRIES,
            backoff_sec: DEFAULT_BACKOFF_SEC,
            interval_sec: None,
            temperature: DEFAULT_TEMPERATURE,
            max_tokens: DEFAULT_CLAUDE_MAX_TOKENS,
        }
    }
}

/// The Anthropic messages API.
pub struct ClaudeProvider {
    base: ProviderBase,
    credential: ClaudeCredential,
    base_url: String,
    timeout_sec: u64,
    temperature: f64,
    max_tokens: i64,
}

impl ClaudeProvider {
    pub fn new(config: ClaudeConfig) -> Self {
        ClaudeProvider {
            base: ProviderBase::new(config.retries, config.backoff_sec, config.interval_sec),
            credential: config.credential,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            timeout_sec: config.timeout_sec,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
        }
    }
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "ClaudeProvider"
    }

    fn base(&self) -> &ProviderBase {
        &self.base
    }

    fn tool_dialect(&self) -> ToolDialect {
        ToolDialect::Anthropic
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
        effort: ReasoningEffort,
    ) -> Result<ProviderResponse> {
        let (system_content, filtered_messages) = convert_messages_for_claude(messages);

        let mut payload = Map::new();
        payload.insert("model".to_string(), json!(model));
        payload.insert("messages".to_string(), json!(filtered_messages));
        payload.insert("max_tokens".to_string(), json!(self.max_tokens));
        payload.insert("temperature".to_string(), json!(self.temperature));
        if !system_content.is_empty() {
            payload.insert("system".to_string(), json!(system_content));
        }
        // The messages API carries the level inside `output_config`, and accepts
        // a narrower set of names than the enum offers; a level this model has
        // no word for is a rejection, which is what the latch is for.
        if let Some(effort) = self.effective_effort(effort) {
            payload.insert(
                "output_config".to_string(),
                json!({"effort": effort.as_str()}),
            );
        }
        attach_tool_schemas(&mut payload, self.effective_dialect(), tools);

        let url = format!("{}/messages", self.base_url);
        let response = http::post_json(
            &url,
            &Value::Object(payload),
            &self.credential.headers(),
            self.timeout_sec,
        )?;

        let tool_calls = parse_anthropic_tool_calls(&response);
        let content = anthropic_text(&response, model, &tool_calls)?;
        Ok(ProviderResponse {
            content,
            model: answering_model(&response, model),
            usage: reported_usage(&response),
            structured: None,
            tool_calls,
            stop_reason: response
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            raw: response,
        })
    }
}
