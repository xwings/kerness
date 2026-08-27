//! LLM backends and the shape of what they answer.
//!
//! A backend is anything that implements [`Provider`]. The trait supplies the
//! parts every backend shares — the retry budget, the empty-reply guard, and
//! the one-way fallback from native tool calling to the text protocol — so a
//! new backend only has to write one request and read one response.

mod claude;
mod custom;
mod openai;
mod openrouter;

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::logging;
use crate::pyfmt;
use crate::tooling::{ToolCall, ToolSpec};
use crate::toolschema::{parse_openai_tool_calls, tool_schemas, ToolDialect};
use crate::utils::retry;

pub use claude::{ClaudeConfig, ClaudeCredential, ClaudeProvider, CLAUDE_BASE_URL};
pub use custom::{CustomConfig, CustomProvider};
pub use openai::{OpenAiConfig, OpenAiProvider, OPENAI_BASE_URL};
pub use openrouter::{OpenRouterConfig, OpenRouterProvider, OPENROUTER_BASE_URL};

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

/// The state [`Provider`]'s supplied methods keep, which a trait cannot hold.
///
/// Every backend owns one and hands it back from [`Provider::base`]. The
/// degrade latch is atomic rather than behind a lock because it only ever
/// moves in one direction, so a racing pair of turns cannot disagree about
/// where it ended up.
#[derive(Debug)]
pub struct ProviderBase {
    retries: u32,
    backoff_sec: f64,
    interval_sec: Option<f64>,
    native_tools_disabled: AtomicBool,
}

impl ProviderBase {
    /// *retries* is the number of **extra** attempts, so `0` still calls once.
    pub fn new(retries: u32, backoff_sec: f64, interval_sec: Option<f64>) -> Self {
        ProviderBase {
            retries,
            backoff_sec,
            interval_sec,
            native_tools_disabled: AtomicBool::new(false),
        }
    }
}

impl Default for ProviderBase {
    fn default() -> Self {
        ProviderBase::new(2, 2.0, None)
    }
}

/// An LLM backend.
///
/// Implementors write [`Provider::chat`] — one request, one response — and
/// declare which native tool dialect they speak. Everything else is supplied.
pub trait Provider: Send + Sync {
    /// What to call this backend in a log line.
    fn name(&self) -> &str;

    /// The retry budget and degrade latch this backend owns.
    fn base(&self) -> &ProviderBase;

    /// Send one chat request.
    ///
    /// *tools* arrives only when the effective dialect can carry it, so an
    /// implementation never has to re-check the latch.
    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse>;

    /// The dialect this backend speaks natively.
    fn tool_dialect(&self) -> ToolDialect {
        ToolDialect::Text
    }

    /// Whether [`Provider::chat`] can actually carry tool specs.
    ///
    /// Always true for a Rust implementation, whose signature says so. The
    /// Python bindings answer it by inspecting the subclass's `chat`, which is
    /// what keeps hand-written test doubles working untouched.
    fn accepts_tools(&self) -> bool {
        true
    }

    /// The dialect to actually use.
    ///
    /// Three tiers, checked in order and never sniffing a successful response
    /// body: the one-way degrade latch, then the declared dialect, then the
    /// capability answer from [`Provider::accepts_tools`].
    fn effective_dialect(&self) -> ToolDialect {
        if self.base().native_tools_disabled.load(Ordering::Relaxed) {
            return ToolDialect::Text;
        }
        let declared = self.tool_dialect();
        if declared == ToolDialect::Text || !self.accepts_tools() {
            return ToolDialect::Text;
        }
        declared
    }

    /// Latch this provider down to the text protocol if *error* means "no tool
    /// support", and report whether it fired.
    ///
    /// One-way: once dropped, the provider never re-attempts native calling for
    /// the rest of its life. Flipping back would put two dialects in one
    /// conversation.
    fn note_native_tools_rejected(&self, error: &Error) -> bool {
        let Error::ProviderHttp {
            status_code, body, ..
        } = error
        else {
            return false;
        };
        if !matches!(status_code, 400 | 404 | 422) || !body.to_lowercase().contains("tool") {
            return false;
        }
        logging::warning(&format!(
            "Provider {} rejected native tool calling (HTTP {status_code}); \
             falling back to the text protocol for the rest of this session.",
            self.name()
        ));
        self.base().native_tools_disabled.store(true, Ordering::Relaxed);
        true
    }

    /// Send a chat request, retrying on failure.
    ///
    /// *purpose* names the call in the failure message. An endpoint that
    /// rejects the tool schemas is retried once without them rather than
    /// failing the turn.
    fn chat_with_retries(
        &self,
        model: &str,
        messages: &[Value],
        purpose: &str,
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        let base = self.base();
        let attempt = || {
            let response = self.chat_dispatch(model, messages, tools)?;
            // A native tool-use response legitimately carries empty text, so
            // emptiness is only an error when there is no tool call either.
            if response.content.trim().is_empty() && response.tool_calls.is_empty() {
                return Err(Error::ProviderEmpty(format!(
                    "Empty response from {model} for {purpose}"
                )));
            }
            Ok(response)
        };
        match retry(attempt, base.retries, base.backoff_sec, base.interval_sec) {
            Ok(response) => Ok(response),
            Err(error) if error.is_provider() => {
                if tools.is_some_and(|tools| !tools.is_empty())
                    && self.note_native_tools_rejected(&error)
                {
                    return self.chat_with_retries(model, messages, purpose, None);
                }
                Err(error)
            }
            Err(error) => {
                logging::warning(&format!("Provider failed for {purpose}: {error}"));
                Err(Error::provider(format!(
                    "All retries exhausted for {purpose}: {error}"
                )))
            }
        }
    }

    /// Call [`Provider::chat`], passing *tools* only when they can be used.
    fn chat_dispatch(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        if tools.is_some_and(|tools| !tools.is_empty())
            && self.effective_dialect() != ToolDialect::Text
        {
            return self.chat(model, messages, tools);
        }
        self.chat(model, messages, None)
    }
}

/// The body every OpenAI-compatible endpoint takes, before the parts that
/// differ between backends.
fn chat_completions_payload(
    model: &str,
    messages: &[Value],
    temperature: f64,
    top_p: f64,
    max_tokens: Option<i64>,
) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("model".to_string(), json!(model));
    payload.insert("messages".to_string(), json!(messages));
    payload.insert("temperature".to_string(), json!(temperature));
    payload.insert("top_p".to_string(), json!(top_p));
    payload.insert("stream".to_string(), json!(false));
    if let Some(max_tokens) = max_tokens {
        payload.insert("max_tokens".to_string(), json!(max_tokens));
    }
    payload
}

/// The `Bearer` credential header pair, plus the JSON content type.
fn bearer_headers(token: &str) -> crate::http::Headers {
    vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
}

/// A body that is not the shape the API documents.
///
/// Refusing it is the point: a [`ProviderResponse`] carrying junk would be put
/// in front of a model on the next turn.
fn unexpected_response(model: &str, response: &Value) -> Error {
    Error::provider(format!(
        "Unexpected response structure from {model}: {}",
        pyfmt::repr(response)
    ))
}

/// The model the backend says answered, which is not always the one asked for.
fn answering_model(response: &Value, requested: &str) -> String {
    response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested)
        .to_string()
}

/// Reported token counts, or nothing when the backend sent none.
fn reported_usage(response: &Value) -> Map<String, Value> {
    response
        .get("usage")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Read content, tool calls, and stop reason from an OpenAI-shaped body.
///
/// `content` is null on a pure tool-call turn, which is why it is coerced
/// rather than indexed strictly.
fn openai_response(response: &Value, model: &str) -> Result<(String, Vec<ToolCall>, String)> {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| unexpected_response(model, response))?;
    let message = choice
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| unexpected_response(model, response))?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let tool_calls = parse_openai_tool_calls(message);
    let content = message.get("content").filter(|content| !content.is_null());
    if content.is_none() && tool_calls.is_empty() {
        return Err(unexpected_response(model, response));
    }
    let content = content
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((content, tool_calls, finish_reason))
}

/// Lift the system prompt out of *messages* for the Claude API.
///
/// Anthropic takes it as a top-level field; leaving it in the messages list is
/// a 400. Every system message is taken, not just the first — dropping a
/// second one would silently lose half the instructions.
pub fn convert_messages_for_claude(messages: &[Value]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut filtered: Vec<Value> = Vec::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) == Some("system") {
            system_parts.push(message.get("content").and_then(Value::as_str).unwrap_or(""));
        } else {
            filtered.push(message.clone());
        }
    }
    (system_parts.join("\n"), filtered)
}

/// Join the text blocks of a Claude response.
///
/// A response that is only `tool_use` blocks has no text at all, which is
/// legitimate — so an empty result is an error only when no tool was called.
fn anthropic_text(response: &Value, model: &str, tool_calls: &[ToolCall]) -> Result<String> {
    let blocks = response
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response(model, response))?;
    let parts: Vec<&str> = blocks
        .iter()
        .filter(|block| {
            block.is_object()
                && block.get("type").and_then(Value::as_str).unwrap_or("text") == "text"
        })
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() && tool_calls.is_empty() {
        return Err(unexpected_response(model, response));
    }
    Ok(parts.join("\n").trim().to_string())
}

/// Add the native tool schemas to *payload*, or leave it alone when there is
/// nothing to send.
///
/// An empty `tools: []` is a 400 at OpenAI, and a latched provider offering
/// schemas again would earn the same 400 every turn.
fn attach_tool_schemas(
    payload: &mut Map<String, Value>,
    dialect: ToolDialect,
    tools: Option<&[ToolSpec]>,
) {
    if let Some(schemas) = tool_schemas(dialect, tools.unwrap_or(&[])) {
        payload.insert("tools".to_string(), Value::Array(schemas));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use serde_json::json;

    use super::*;
    use crate::http::{self, Headers, HttpTransport};
    use crate::tooling::Arguments;

    /// One captured request.
    struct Call {
        url: String,
        payload: Value,
        headers: Headers,
    }

    /// A transport that records what was sent and replays canned answers.
    ///
    /// The last queued answer repeats, so a test that only cares about the
    /// request queues one reply and calls as often as it likes.
    struct Recorder {
        calls: Mutex<Vec<Call>>,
        replies: Mutex<VecDeque<Result<Value>>>,
    }

    impl HttpTransport for Recorder {
        fn post_json(
            &self,
            url: &str,
            payload: &Value,
            headers: &Headers,
            _timeout_sec: u64,
        ) -> Result<Value> {
            self.calls.lock().unwrap().push(Call {
                url: url.to_string(),
                payload: payload.clone(),
                headers: headers.clone(),
            });
            let mut replies = self.replies.lock().unwrap();
            let reply = replies.front().expect("no reply queued").clone();
            if replies.len() > 1 {
                replies.pop_front();
            }
            reply
        }
    }

    impl Recorder {
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        /// The most recent request, as `(url, payload, headers)`.
        fn last(&self) -> (String, Value, Headers) {
            let calls = self.calls.lock().unwrap();
            let call = calls.last().expect("no request was sent");
            (call.url.clone(), call.payload.clone(), call.headers.clone())
        }

        fn payload(&self) -> Value {
            self.last().1
        }

        fn header(&self, name: &str) -> Option<String> {
            self.last()
                .2
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The transport is process-global, so provider tests take turns.
    fn install(replies: Vec<Result<Value>>) -> (MutexGuard<'static, ()>, Arc<Recorder>) {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let recorder = Arc::new(Recorder {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        });
        http::set_transport(recorder.clone());
        (guard, recorder)
    }

    /// The OpenAI-shaped envelope every chat-completions mock has to return.
    fn reply(content: &str, model: &str, usage: Value) -> Result<Value> {
        Ok(json!({
            "choices": [{"message": {"content": content}}],
            "model": model,
            "usage": usage,
        }))
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn cmd_spec() -> ToolSpec {
        ToolSpec::new(
            "cmd",
            "Run a shell command.",
            json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            Arc::new(|_: &Arguments, _: &str| Ok(String::new())),
        )
    }

    fn openrouter(api_key: &str) -> OpenRouterProvider {
        OpenRouterProvider::new(OpenRouterConfig {
            api_key: api_key.to_string(),
            ..OpenRouterConfig::default()
        })
    }

    fn openai(api_key: &str) -> OpenAiProvider {
        OpenAiProvider::new(OpenAiConfig {
            api_key: api_key.to_string(),
            ..OpenAiConfig::default()
        })
        .unwrap()
    }

    fn claude(api_key: &str) -> ClaudeProvider {
        ClaudeProvider::new(ClaudeConfig {
            credential: ClaudeCredential::ApiKey(api_key.to_string()),
            ..ClaudeConfig::default()
        })
    }

    fn custom(url: &str) -> CustomProvider {
        CustomProvider::new(CustomConfig {
            url: url.to_string(),
            api_key: "sk-test".to_string(),
            ..CustomConfig::default()
        })
    }

    #[test]
    fn only_content_is_required() {
        let bare = ProviderResponse::text("hi");
        assert_eq!(bare.content, "hi");
        assert_eq!(bare.model, "");
        assert!(bare.usage.is_empty());
        assert_eq!(bare.raw, Value::Null);
        assert_eq!(bare.structured, None);
    }

    #[test]
    fn openrouter_sends_its_attribution_and_reports_the_model_that_answered() {
        let (_guard, recorder) = install(vec![reply(
            "  parsed reply  ",
            "actual/model",
            json!({"prompt_tokens": 5}),
        )]);
        let provider = OpenRouterProvider::new(OpenRouterConfig {
            api_key: "sk-test".to_string(),
            app_name: "TestApp".to_string(),
            app_url: "http://test.com".to_string(),
            ..OpenRouterConfig::default()
        });
        let response = provider
            .chat("test/model", &[user("hi")], None)
            .expect("the reply is well formed");

        let (url, payload, _) = recorder.last();
        assert!(url.contains("chat/completions"));
        assert_eq!(recorder.header("Authorization").unwrap(), "Bearer sk-test");
        assert_eq!(recorder.header("X-Title").unwrap(), "TestApp");
        assert_eq!(recorder.header("HTTP-Referer").unwrap(), "http://test.com");
        assert_eq!(payload["model"], json!("test/model"));
        assert_eq!(payload["messages"], json!([user("hi")]));

        assert_eq!(response.content, "parsed reply");
        assert_eq!(response.model, "actual/model");
        assert_eq!(response.usage["prompt_tokens"], json!(5));
    }

    #[test]
    fn a_plain_openai_turn_is_untouched() {
        let (_guard, recorder) = install(vec![reply("  openai reply  ", "gpt-4o", json!({}))]);
        let response = openai("sk-test")
            .chat("gpt-4o", &[user("hi")], None)
            .expect("the reply is well formed");

        let (url, payload, _) = recorder.last();
        assert!(url.contains("api.openai.com"));
        assert!(url.contains("chat/completions"));
        assert_eq!(recorder.header("Authorization").unwrap(), "Bearer sk-test");
        assert_eq!(payload["model"], json!("gpt-4o"));
        assert_eq!(payload.get("response_format"), None);

        assert_eq!(response.content, "openai reply");
        assert_eq!(response.structured, None);
    }

    /// The schema pydantic emits for a two-field model, which is what the
    /// bindings hand down.
    fn answer_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}, "score": {"type": "integer"}},
            "required": ["answer", "score"],
        })
    }

    #[test]
    fn structured_output_builds_a_response_format() {
        let (_guard, recorder) = install(vec![reply(
            r#"{"answer":"yes","score":9}"#,
            "gpt-4o",
            json!({}),
        )]);
        let provider = OpenAiProvider::new(OpenAiConfig {
            api_key: "sk-test".to_string(),
            output_schema: Some(answer_schema()),
            output_schema_name: "my_schema".to_string(),
            ..OpenAiConfig::default()
        })
        .unwrap();
        provider.chat("gpt-4o", &[user("hi")], None).unwrap();

        let format = recorder.payload()["response_format"].clone();
        assert_eq!(format["type"], json!("json_schema"));
        assert_eq!(format["json_schema"]["name"], json!("my_schema"));
        assert_eq!(format["json_schema"]["strict"], json!(true));
        let schema = &format["json_schema"]["schema"];
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["answer", "score"]));
    }

    #[test]
    fn an_unnamed_schema_is_still_named() {
        let (_guard, recorder) = install(vec![reply(
            r#"{"answer":"yes","score":9}"#,
            "gpt-4o",
            json!({}),
        )]);
        OpenAiProvider::new(OpenAiConfig {
            api_key: "sk-test".to_string(),
            output_schema: Some(answer_schema()),
            ..OpenAiConfig::default()
        })
        .unwrap()
        .chat("gpt-4o", &[], None)
        .unwrap();

        assert_eq!(
            recorder.payload()["response_format"]["json_schema"]["name"],
            json!("response_format")
        );
    }

    #[test]
    fn strict_mode_is_what_rewrites_the_schema() {
        for strict in [false, true] {
            let (_guard, recorder) =
                install(vec![reply(r#"{"answer":"yes"}"#, "gpt-4o", json!({}))]);
            OpenAiProvider::new(OpenAiConfig {
                api_key: "sk-test".to_string(),
                output_schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "answer": {"type": "string"},
                        "note": {"type": ["string", "null"], "default": null},
                    },
                    "required": ["answer"],
                })),
                strict_json_schema: strict,
                ..OpenAiConfig::default()
            })
            .unwrap()
            .chat("gpt-4o", &[], None)
            .unwrap();

            let format = recorder.payload()["response_format"]["json_schema"].clone();
            assert_eq!(format["strict"], json!(strict));
            let schema = &format["schema"];
            if strict {
                assert_eq!(schema["required"], json!(["answer", "note"]));
                assert_eq!(schema["additionalProperties"], json!(false));
                assert_eq!(schema["properties"]["note"].get("default"), None);
            } else {
                assert_eq!(schema["required"], json!(["answer"]));
                assert_eq!(schema.get("additionalProperties"), None);
                assert_eq!(schema["properties"]["note"]["default"], json!(null));
            }
        }
    }

    #[test]
    fn a_valid_reply_is_parsed_and_the_text_is_kept_beside_it() {
        let (_guard, _recorder) = install(vec![reply(
            r#"  {"answer":"ok","score":7}  "#,
            "gpt-4o",
            json!({}),
        )]);
        let response = OpenAiProvider::new(OpenAiConfig {
            api_key: "sk-test".to_string(),
            output_schema: Some(answer_schema()),
            ..OpenAiConfig::default()
        })
        .unwrap()
        .chat("gpt-4o", &[], None)
        .unwrap();

        assert_eq!(response.content, r#"{"answer":"ok","score":7}"#);
        assert_eq!(
            response.structured,
            Some(json!({"answer": "ok", "score": 7}))
        );
    }

    #[test]
    fn a_reply_that_is_not_json_is_reported_with_the_response_shape() {
        let (_guard, _recorder) = install(vec![reply("not json", "gpt-4o", json!({}))]);
        let error = OpenAiProvider::new(OpenAiConfig {
            api_key: "sk-test".to_string(),
            output_schema: Some(answer_schema()),
            ..OpenAiConfig::default()
        })
        .unwrap()
        .chat("gpt-4o", &[], None)
        .expect_err("the body is not the schema");

        let message = error.to_string();
        assert!(
            message.starts_with("Structured output parsing failed for gpt-4o: "),
            "{message}"
        );
        assert!(
            message.ends_with("Response shape: {'keys': ['choices', 'model', 'usage'], 'choice_count': 1}"),
            "{message}"
        );
    }

    #[test]
    fn claude_takes_the_system_prompt_as_its_own_field() {
        let (_guard, recorder) = install(vec![Ok(json!({
            "content": [{"text": "  claude reply  "}],
            "model": "claude-sonnet-4-20250514",
            "usage": {"input_tokens": 10},
        }))]);
        let response = claude("sk-ant-test")
            .chat(
                "claude-sonnet-4-20250514",
                &[
                    json!({"role": "system", "content": "You are helpful."}),
                    user("hi"),
                ],
                None,
            )
            .expect("the reply is well formed");

        let (url, payload, _) = recorder.last();
        assert!(url.contains("api.anthropic.com"));
        assert!(url.contains("/messages"));
        assert_eq!(recorder.header("x-api-key").unwrap(), "sk-ant-test");
        assert_eq!(recorder.header("anthropic-version").unwrap(), "2023-06-01");
        assert_eq!(payload["system"], json!("You are helpful."));
        assert_eq!(payload["messages"], json!([user("hi")]));

        assert_eq!(response.content, "claude reply");
    }

    #[test]
    fn an_oauth_credential_replaces_the_api_key_header() {
        let (_guard, recorder) = install(vec![Ok(json!({
            "content": [{"text": "reply"}],
            "model": "claude-sonnet-4-20250514",
            "usage": {},
        }))]);
        ClaudeProvider::new(ClaudeConfig {
            credential: ClaudeCredential::OAuth("oauth-token-456".to_string()),
            ..ClaudeConfig::default()
        })
        .chat("claude-sonnet-4-20250514", &[user("hi")], None)
        .unwrap();

        assert_eq!(
            recorder.header("Authorization").unwrap(),
            "Bearer oauth-token-456"
        );
        assert_eq!(recorder.header("x-api-key"), None);
        assert_eq!(recorder.header("anthropic-version").unwrap(), "2023-06-01");
    }

    #[test]
    fn every_system_message_is_lifted_out_and_none_is_lost() {
        let (system, filtered) = convert_messages_for_claude(&[
            json!({"role": "system", "content": "Be helpful."}),
            user("hi"),
            json!({"role": "assistant", "content": "hello"}),
        ]);
        assert_eq!(system, "Be helpful.");
        assert_eq!(filtered.len(), 2);

        let (system, filtered) = convert_messages_for_claude(&[
            json!({"role": "system", "content": "First."}),
            json!({"role": "system", "content": "Second."}),
            user("hi"),
        ]);
        assert_eq!(system, "First.\nSecond.");
        assert_eq!(filtered.len(), 1);

        let (system, filtered) = convert_messages_for_claude(&[user("hi")]);
        assert_eq!(system, "");
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn the_budget_is_spent_only_on_failure_and_then_reported() {
        let provider = OpenRouterProvider::new(OpenRouterConfig {
            api_key: "sk-test".to_string(),
            retries: 2,
            backoff_sec: 0.0,
            ..OpenRouterConfig::default()
        });

        let (guard, recorder) = install(vec![reply("ok", "m", json!({}))]);
        assert_eq!(
            provider
                .chat_with_retries("m", &[], "test", None)
                .unwrap()
                .content,
            "ok"
        );
        assert_eq!(recorder.count(), 1);
        drop(guard);

        let (guard, recorder) = install(vec![
            Err(Error::session("fail")),
            reply("ok", "m", json!({})),
        ]);
        assert_eq!(
            provider
                .chat_with_retries("m", &[], "test", None)
                .unwrap()
                .content,
            "ok"
        );
        assert_eq!(recorder.count(), 2);
        drop(guard);

        let (_guard, _recorder) = install(vec![Err(Error::session("always fail"))]);
        let error = provider
            .chat_with_retries("m", &[], "test", None)
            .expect_err("the budget runs out");
        assert!(
            error.to_string().starts_with("All retries exhausted"),
            "{error}"
        );
    }

    #[test]
    fn a_custom_endpoint_is_a_bearer_token_and_a_joined_url() {
        let (_guard, recorder) = install(vec![reply(
            "  custom reply  ",
            "actual-model",
            json!({"prompt_tokens": 10}),
        )]);
        let response = custom("https://coding.dashscope.aliyuncs.com/v1/")
            .chat("qwen3.5-plus", &[user("hi")], None)
            .expect("the reply is well formed");

        let (url, payload, _) = recorder.last();
        assert_eq!(
            url,
            "https://coding.dashscope.aliyuncs.com/v1/chat/completions"
        );
        assert_eq!(recorder.header("Authorization").unwrap(), "Bearer sk-test");
        assert_eq!(payload["model"], json!("qwen3.5-plus"));
        assert_eq!(payload["stream"], json!(false));
        assert_eq!(payload.get("max_tokens"), None);

        assert_eq!(response.content, "custom reply");
        assert_eq!(response.model, "actual-model");
        assert_eq!(response.usage["prompt_tokens"], json!(10));
    }

    #[test]
    fn an_explicit_max_tokens_outranks_the_model_config() {
        for (max_tokens, expected) in [(None, 65536), (Some(4096), 4096)] {
            let (_guard, recorder) = install(vec![reply("hi", "m", json!({}))]);
            CustomProvider::new(CustomConfig {
                url: "https://example.com/v1".to_string(),
                api_key: "sk-test".to_string(),
                model_config: json!({"id": "qwen3.5-plus", "maxTokens": 65536})
                    .as_object()
                    .unwrap()
                    .clone(),
                max_tokens,
                ..CustomConfig::default()
            })
            .chat("m", &[], None)
            .unwrap();

            assert_eq!(recorder.payload()["max_tokens"], json!(expected));
        }
    }

    #[test]
    fn extra_headers_and_body_are_merged_not_replaced() {
        let (_guard, recorder) = install(vec![reply("hi", "m", json!({}))]);
        CustomProvider::new(CustomConfig {
            url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            extra_headers: vec![("X-Custom".to_string(), "value".to_string())],
            extra_body: json!({"enable_search": true, "top_k": 50})
                .as_object()
                .unwrap()
                .clone(),
            ..CustomConfig::default()
        })
        .chat("m", &[], None)
        .unwrap();

        assert_eq!(recorder.header("X-Custom").unwrap(), "value");
        assert_eq!(recorder.header("Authorization").unwrap(), "Bearer sk-test");
        let payload = recorder.payload();
        assert_eq!(payload["enable_search"], json!(true));
        assert_eq!(payload["top_k"], json!(50));
    }

    #[test]
    fn every_provider_refuses_a_body_it_cannot_read() {
        let (_guard, _recorder) = install(vec![Ok(json!({"error": "bad request"}))]);
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(openrouter("sk-test")),
            Box::new(claude("sk-ant-test")),
            Box::new(custom("https://example.com/v1")),
        ];
        for provider in providers {
            let error = provider
                .chat("m", &[user("hi")], None)
                .expect_err("the body is not a reply");
            assert!(
                error.to_string().starts_with("Unexpected response"),
                "{}: {error}",
                provider.name()
            );
        }
    }

    #[test]
    fn a_declared_dialect_wins_when_chat_can_carry_tools() {
        assert_eq!(openai("k").effective_dialect(), ToolDialect::Openai);
        assert_eq!(claude("k").effective_dialect(), ToolDialect::Anthropic);
    }

    #[test]
    fn a_chat_that_cannot_carry_tools_falls_back_to_text() {
        struct ToolsUnaware(ProviderBase);
        impl Provider for ToolsUnaware {
            fn name(&self) -> &str {
                "ToolsUnaware"
            }
            fn base(&self) -> &ProviderBase {
                &self.0
            }
            fn tool_dialect(&self) -> ToolDialect {
                ToolDialect::Openai
            }
            fn accepts_tools(&self) -> bool {
                false
            }
            fn chat(&self, model: &str, _: &[Value], _: Option<&[ToolSpec]>) -> Result<ProviderResponse> {
                Ok(ProviderResponse {
                    model: model.to_string(),
                    ..ProviderResponse::text("hi")
                })
            }
        }

        assert_eq!(
            ToolsUnaware(ProviderBase::default()).effective_dialect(),
            ToolDialect::Text
        );
    }

    #[test]
    fn a_400_naming_tools_latches_down_to_text_for_good() {
        let provider = openai("k");
        let rejection = Error::ProviderHttp {
            status_code: 400,
            url: "https://x".to_string(),
            body: "tools is not supported".to_string(),
        };

        assert!(provider.note_native_tools_rejected(&rejection));
        assert_eq!(provider.effective_dialect(), ToolDialect::Text);
        assert_eq!(provider.effective_dialect(), ToolDialect::Text);
    }

    #[test]
    fn a_failure_that_is_not_about_tools_does_not_latch() {
        let provider = openai("k");
        assert!(!provider.note_native_tools_rejected(&Error::ProviderHttp {
            status_code: 400,
            url: "https://x".to_string(),
            body: "invalid api key".to_string(),
        }));
        assert!(!provider.note_native_tools_rejected(&Error::ProviderHttp {
            status_code: 500,
            url: "https://x".to_string(),
            body: "tools broke".to_string(),
        }));
        assert_eq!(provider.effective_dialect(), ToolDialect::Openai);
    }

    #[test]
    fn each_dialect_sends_its_own_schema_shape() {
        let (guard, recorder) = install(vec![reply("ok", "m", json!({}))]);
        openai("k").chat("m", &[], Some(&[cmd_spec()])).unwrap();
        assert_eq!(
            recorder.payload()["tools"][0]["function"]["name"],
            json!("cmd")
        );
        drop(guard);

        let (_guard, recorder) = install(vec![Ok(json!({
            "content": [{"text": "ok"}], "model": "m",
        }))]);
        claude("k").chat("m", &[], Some(&[cmd_spec()])).unwrap();
        assert_eq!(
            recorder.payload()["tools"][0]["input_schema"],
            cmd_spec().parameters
        );
    }

    #[test]
    fn no_tools_key_when_there_is_nothing_to_send() {
        let (guard, recorder) = install(vec![reply("ok", "m", json!({}))]);
        openai("k").chat("m", &[], None).unwrap();
        assert_eq!(recorder.payload().get("tools"), None);
        drop(guard);

        let (_guard, recorder) = install(vec![reply("ok", "m", json!({}))]);
        let latched = openai("k");
        latched.note_native_tools_rejected(&Error::ProviderHttp {
            status_code: 400,
            url: "https://x".to_string(),
            body: "tools unsupported".to_string(),
        });
        latched.chat("m", &[], Some(&[cmd_spec()])).unwrap();
        assert_eq!(recorder.payload().get("tools"), None);
    }

    #[test]
    fn a_tool_call_with_null_content_is_a_turn() {
        let (_guard, _recorder) = install(vec![Ok(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "c1", "type": "function",
                        "function": {"name": "cmd", "arguments": "{\"command\":\"ls\"}"},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "model": "m",
        }))]);
        let response = openai("k").chat("m", &[], None).unwrap();

        assert_eq!(response.content, "");
        assert_eq!(
            response.tool_calls,
            vec![ToolCall::new(
                "cmd",
                json!({"command": "ls"}).as_object().unwrap().clone()
            )
            .with_id("c1")]
        );
        assert_eq!(response.stop_reason, "tool_calls");
    }

    #[test]
    fn claude_reads_every_block_in_the_content_list() {
        let (guard, _recorder) = install(vec![Ok(json!({
            "content": [{"type": "tool_use", "id": "tu_1", "name": "cmd",
                         "input": {"command": "ls"}}],
            "model": "m",
            "stop_reason": "tool_use",
        }))]);
        let response = claude("k").chat("m", &[], None).unwrap();
        assert_eq!(response.content, "");
        assert_eq!(
            response.tool_calls,
            vec![ToolCall::new(
                "cmd",
                json!({"command": "ls"}).as_object().unwrap().clone()
            )
            .with_id("tu_1")]
        );
        assert_eq!(response.stop_reason, "tool_use");
        drop(guard);

        let (_guard, _recorder) = install(vec![Ok(json!({
            "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}],
            "model": "m",
        }))]);
        assert_eq!(claude("k").chat("m", &[], None).unwrap().content, "one\ntwo");
    }

    #[test]
    fn structured_output_is_skipped_on_a_tool_calling_turn() {
        let (_guard, _recorder) = install(vec![Ok(json!({
            "choices": [{"message": {
                "content": null,
                "tool_calls": [{"id": "c1", "function": {"name": "cmd", "arguments": "{}"}}],
            }}],
            "model": "m",
        }))]);
        let response = OpenAiProvider::new(OpenAiConfig {
            api_key: "k".to_string(),
            output_schema: Some(answer_schema()),
            ..OpenAiConfig::default()
        })
        .unwrap()
        .chat("m", &[], None)
        .unwrap();

        assert_eq!(response.structured, None);
        assert!(!response.tool_calls.is_empty());
    }

    #[test]
    fn it_is_the_tool_call_and_not_the_text_that_makes_a_turn_real() {
        let (guard, recorder) = install(vec![Ok(json!({
            "content": [{"type": "tool_use", "id": "tu_1", "name": "cmd", "input": {}}],
            "model": "m",
        }))]);
        let provider = ClaudeProvider::new(ClaudeConfig {
            credential: ClaudeCredential::ApiKey("k".to_string()),
            retries: 0,
            backoff_sec: 0.0,
            ..ClaudeConfig::default()
        });
        let response = provider
            .chat_with_retries("m", &[], "turn", Some(&[cmd_spec()]))
            .expect("a tool-use turn carries no text");
        assert_eq!(response.content, "");
        assert_eq!(recorder.count(), 1);
        drop(guard);

        let (_guard, _recorder) = install(vec![Ok(json!({
            "choices": [{"message": {"content": "   "}}], "model": "m",
        }))]);
        let error = OpenAiProvider::new(OpenAiConfig {
            api_key: "k".to_string(),
            retries: 0,
            backoff_sec: 0.0,
            ..OpenAiConfig::default()
        })
        .unwrap()
        .chat_with_retries("m", &[], "turn", None)
        .expect_err("an empty reply beside nothing at all is a failure");
        assert!(error.is_provider(), "{error}");
    }

    #[test]
    fn a_rejected_endpoint_retries_once_without_tools() {
        let (_guard, recorder) = install(vec![
            Err(Error::ProviderHttp {
                status_code: 400,
                url: "https://x".to_string(),
                body: "tools is not supported".to_string(),
            }),
            Ok(json!({"choices": [{"message": {"content": "plain reply"}}], "model": "m"})),
        ]);
        let provider = CustomProvider::new(CustomConfig {
            url: "https://x/v1".to_string(),
            api_key: "k".to_string(),
            retries: 0,
            backoff_sec: 0.0,
            ..CustomConfig::default()
        });
        let response = provider
            .chat_with_retries("m", &[], "turn", Some(&[cmd_spec()]))
            .expect("the degrade latch salvages the turn");

        assert_eq!(response.content, "plain reply");
        assert_eq!(recorder.payload().get("tools"), None);
    }
}
