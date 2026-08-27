//! The OpenRouter backend.

use serde_json::Value;

use super::{
    answering_model, attach_tool_schemas, bearer_headers, chat_completions_payload, openai_response,
    reported_usage, Provider, ProviderBase, ProviderResponse,
};
use crate::error::Result;
use crate::http;
use crate::tooling::ToolSpec;
use crate::toolschema::ToolDialect;

/// Where the API lives when the caller names no other host.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Everything [`OpenRouterProvider`] is built from.
pub struct OpenRouterConfig {
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
    /// Sent as `HTTP-Referer`; one half of what OpenRouter ranks apps by.
    pub app_url: String,
    /// Sent as `X-Title`; the other half.
    pub app_name: String,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        OpenRouterConfig {
            api_key: String::new(),
            base_url: OPENROUTER_BASE_URL.to_string(),
            timeout_sec: 60,
            retries: 2,
            backoff_sec: 2.0,
            interval_sec: None,
            temperature: 1.0,
            top_p: 1.0,
            max_tokens: None,
            app_url: String::new(),
            app_name: String::new(),
        }
    }
}

/// The OpenRouter API.
pub struct OpenRouterProvider {
    base: ProviderBase,
    api_key: String,
    base_url: String,
    timeout_sec: u64,
    temperature: f64,
    top_p: f64,
    max_tokens: Option<i64>,
    app_url: String,
    app_name: String,
}

impl OpenRouterProvider {
    pub fn new(config: OpenRouterConfig) -> Self {
        OpenRouterProvider {
            base: ProviderBase::new(config.retries, config.backoff_sec, config.interval_sec),
            api_key: config.api_key,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            timeout_sec: config.timeout_sec,
            temperature: config.temperature,
            top_p: config.top_p,
            max_tokens: config.max_tokens,
            app_url: config.app_url,
            app_name: config.app_name,
        }
    }
}

impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        "OpenRouterProvider"
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
    ) -> Result<ProviderResponse> {
        let mut headers = bearer_headers(&self.api_key);
        if !self.app_url.is_empty() {
            headers.push(("HTTP-Referer".to_string(), self.app_url.clone()));
        }
        if !self.app_name.is_empty() {
            headers.push(("X-Title".to_string(), self.app_name.clone()));
        }

        let mut payload = chat_completions_payload(
            model,
            messages,
            self.temperature,
            self.top_p,
            self.max_tokens,
        );
        attach_tool_schemas(&mut payload, self.effective_dialect(), tools);

        let url = format!("{}/chat/completions", self.base_url);
        let response = http::post_json(&url, &Value::Object(payload), &headers, self.timeout_sec)?;

        let (content, tool_calls, stop_reason) = openai_response(&response, model)?;
        Ok(ProviderResponse {
            content,
            model: answering_model(&response, model),
            usage: reported_usage(&response),
            structured: None,
            tool_calls,
            stop_reason,
            raw: response,
        })
    }
}
