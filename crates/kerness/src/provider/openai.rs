//! The OpenAI chat-completions backend.

use serde_json::{json, Map, Value};

use super::{
    attach_reasoning_effort, attach_tool_schemas, bearer_headers, chat_completions_payload,
    post_chat_completions, Provider, ProviderBase, ProviderResponse, ReasoningEffort,
    DEFAULT_BACKOFF_SEC, DEFAULT_REQUEST_TIMEOUT_SEC, DEFAULT_RETRIES, DEFAULT_TEMPERATURE,
    DEFAULT_TOP_P,
};
use crate::error::{Error, Result};
use crate::jsonschema::ensure_strict;
use crate::pyfmt;
use crate::tooling::ToolSpec;
use crate::toolschema::ToolDialect;

/// Where the API lives when the caller names no other host.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Everything [`OpenAiProvider`] is built from.
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout_sec: u64,
    /// Extra attempts after the first, so `0` still calls once.
    pub retries: u32,
    pub backoff_sec: f64,
    pub interval_sec: Option<f64>,
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: Option<i64>,
    /// JSON Schema the reply must satisfy, or `None` for a free-text turn.
    ///
    /// The bindings derive this from the caller's pydantic model; a Rust
    /// caller writes it directly.
    pub output_schema: Option<Value>,
    /// Whether to rewrite *output_schema* into OpenAI's strict subset.
    pub strict_json_schema: bool,
    /// The name the schema is sent under. Empty means `response_format`.
    pub output_schema_name: String,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        OpenAiConfig {
            api_key: String::new(),
            base_url: OPENAI_BASE_URL.to_string(),
            timeout_sec: DEFAULT_REQUEST_TIMEOUT_SEC,
            retries: DEFAULT_RETRIES,
            backoff_sec: DEFAULT_BACKOFF_SEC,
            interval_sec: None,
            temperature: DEFAULT_TEMPERATURE,
            top_p: DEFAULT_TOP_P,
            max_tokens: None,
            output_schema: None,
            strict_json_schema: true,
            output_schema_name: String::new(),
        }
    }
}

/// The OpenAI API.
pub struct OpenAiProvider {
    base: ProviderBase,
    api_key: String,
    base_url: String,
    timeout_sec: u64,
    temperature: f64,
    top_p: f64,
    max_tokens: Option<i64>,
    /// Built once at construction: the schema never changes between turns, so
    /// rewriting and re-wrapping it per request would be work for nothing.
    response_format: Option<Value>,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        let mut response_format = None;
        if let Some(mut schema) = config.output_schema {
            if config.strict_json_schema {
                ensure_strict(&mut schema)?;
            }
            let name = if config.output_schema_name.is_empty() {
                "response_format"
            } else {
                &config.output_schema_name
            };
            response_format = Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": name,
                    "strict": config.strict_json_schema,
                    "schema": schema,
                },
            }));
        }
        Ok(OpenAiProvider {
            base: ProviderBase::new(config.retries, config.backoff_sec, config.interval_sec),
            api_key: config.api_key,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            timeout_sec: config.timeout_sec,
            temperature: config.temperature,
            top_p: config.top_p,
            max_tokens: config.max_tokens,
            response_format,
        })
    }
}

impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "OpenAiProvider"
    }

    fn base(&self) -> &ProviderBase {
        &self.base
    }

    fn tool_dialect(&self) -> ToolDialect {
        ToolDialect::Openai
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
        effort: ReasoningEffort,
    ) -> Result<ProviderResponse> {
        let mut payload = chat_completions_payload(
            model,
            messages,
            self.temperature,
            self.top_p,
            self.max_tokens,
        );
        if let Some(response_format) = &self.response_format {
            payload.insert("response_format".to_string(), response_format.clone());
        }
        attach_reasoning_effort(&mut payload, self.effective_effort(effort));
        attach_tool_schemas(&mut payload, self.effective_dialect(), tools);

        let mut answer = post_chat_completions(
            &self.base_url,
            payload,
            &bearer_headers(&self.api_key),
            self.timeout_sec,
            model,
        )?;
        // A tool-calling turn has no JSON body to validate; the structured
        // answer comes on the turn after the results are fed back.
        if self.response_format.is_some() && answer.tool_calls.is_empty() {
            answer.structured = Some(decode_structured(&answer.content, &answer.raw, model)?);
        }
        Ok(answer)
    }
}

/// Decode the reply as JSON, naming the response shape when it is not.
///
/// The shape rather than the body: a reply that failed to parse is exactly the
/// text a log should not be filled with, and the key list is enough to tell a
/// truncated response from a refusal.
fn decode_structured(content: &str, response: &Value, model: &str) -> Result<Value> {
    serde_json::from_str(content).map_err(|err| {
        let keys: Vec<Value> = response
            .as_object()
            .map(|body| body.keys().map(|key| json!(key)).collect())
            .unwrap_or_default();
        let choice_count = response
            .get("choices")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let mut shape = Map::new();
        shape.insert("keys".to_string(), Value::Array(keys));
        shape.insert("choice_count".to_string(), json!(choice_count));
        Error::provider(format!(
            "Structured output parsing failed for {model}: {err}. Response shape: {}",
            pyfmt::repr(&Value::Object(shape))
        ))
    })
}
