//! Any OpenAI-compatible endpoint.

use serde_json::{Map, Value};

use super::{
    attach_reasoning_effort, attach_tool_schemas, bearer_headers, chat_completions_payload,
    post_chat_completions, Provider, ProviderBase, ProviderResponse, ReasoningEffort,
    DEFAULT_BACKOFF_SEC, DEFAULT_REQUEST_TIMEOUT_SEC, DEFAULT_RETRIES, DEFAULT_TEMPERATURE,
    DEFAULT_TOP_P,
};
use crate::error::Result;
use crate::http::Headers;
use crate::tooling::ToolSpec;
use crate::toolschema::ToolDialect;

/// Everything [`CustomProvider`] is built from.
pub struct CustomConfig {
    /// Base URL of the API, with or without a trailing slash.
    pub url: String,
    pub api_key: String,
    /// The vendor's own model description, verbatim. Only `maxTokens` is read,
    /// as the fallback for *max_tokens*; the rest is kept for the caller.
    pub model_config: Map<String, Value>,
    pub timeout_sec: u64,
    /// Extra attempts after the first, so `0` still calls once.
    pub retries: u32,
    pub backoff_sec: f64,
    pub interval_sec: Option<f64>,
    pub temperature: f64,
    pub top_p: f64,
    /// Outranks `model_config["maxTokens"]` when set.
    pub max_tokens: Option<i64>,
    /// Merged into every request's headers.
    pub extra_headers: Headers,
    /// Merged into every request's body.
    pub extra_body: Map<String, Value>,
}

impl Default for CustomConfig {
    fn default() -> Self {
        CustomConfig {
            url: String::new(),
            api_key: String::new(),
            model_config: Map::new(),
            timeout_sec: DEFAULT_REQUEST_TIMEOUT_SEC,
            retries: DEFAULT_RETRIES,
            backoff_sec: DEFAULT_BACKOFF_SEC,
            interval_sec: None,
            temperature: DEFAULT_TEMPERATURE,
            top_p: DEFAULT_TOP_P,
            max_tokens: None,
            extra_headers: Vec::new(),
            extra_body: Map::new(),
        }
    }
}

/// A vendor endpoint that speaks the OpenAI chat-completions protocol.
///
/// The dialect is assumed to be OpenAI's, since that is what "OpenAI-compatible"
/// means. An endpoint that advertises compatibility without implementing
/// function calling degrades to the text protocol on its first 400 — see
/// [`Provider::note_native_tools_rejected`].
pub struct CustomProvider {
    base: ProviderBase,
    base_url: String,
    api_key: String,
    model_config: Map<String, Value>,
    timeout_sec: u64,
    temperature: f64,
    top_p: f64,
    max_tokens: Option<i64>,
    extra_headers: Headers,
    extra_body: Map<String, Value>,
}

impl CustomProvider {
    pub fn new(config: CustomConfig) -> Self {
        let max_tokens = config.max_tokens.or_else(|| {
            config
                .model_config
                .get("maxTokens")
                .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|n| n as i64)))
        });
        CustomProvider {
            base: ProviderBase::new(config.retries, config.backoff_sec, config.interval_sec),
            base_url: config.url.trim_end_matches('/').to_string(),
            api_key: config.api_key,
            model_config: config.model_config,
            timeout_sec: config.timeout_sec,
            temperature: config.temperature,
            top_p: config.top_p,
            max_tokens,
            extra_headers: config.extra_headers,
            extra_body: config.extra_body,
        }
    }

    /// The vendor's model description, as it was supplied.
    pub fn model_config(&self) -> &Map<String, Value> {
        &self.model_config
    }
}

impl Provider for CustomProvider {
    fn name(&self) -> &str {
        "CustomProvider"
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
        let mut headers = bearer_headers(&self.api_key);
        for (name, value) in &self.extra_headers {
            match headers.iter_mut().find(|(existing, _)| existing == name) {
                Some(slot) => slot.1 = value.clone(),
                None => headers.push((name.clone(), value.clone())),
            }
        }

        let mut payload = chat_completions_payload(
            model,
            messages,
            self.temperature,
            self.top_p,
            self.max_tokens,
        );
        // Before the extra body, unlike the tool schemas: a vendor that spells
        // the level its own way needs to be able to overwrite this key.
        attach_reasoning_effort(&mut payload, self.effective_effort(effort));
        for (key, value) in &self.extra_body {
            payload.insert(key.clone(), value.clone());
        }
        // After the extra body, so a vendor cannot override the schemas the
        // framework needs the model to see.
        attach_tool_schemas(&mut payload, self.effective_dialect(), tools);

        post_chat_completions(&self.base_url, payload, &headers, self.timeout_sec, model)
    }
}
