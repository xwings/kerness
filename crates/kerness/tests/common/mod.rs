//! Doubles the integration tests drive the framework with.
//!
//! The unit tests inside `src/` reach into module internals. These tests may
//! not: they compile against the crate exactly as a dependent does, so a
//! session here is assembled the way `examples/debate.rs` assembles one, and a
//! break in the public surface fails a test rather than a downstream build.
//!
//! What that costs is a provider and a channel that answer without a network,
//! and a temporary directory to write session files into. All three live here,
//! matching what `tests/conftest.py` gives the Python suite.

// Each test binary compiles this module and uses part of it. Rust warns about
// the rest, which would be 30 warnings per binary about code another binary
// depends on.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Value};

use kerness::channel::Channel;
use kerness::error::Result;
use kerness::provider::{Provider, ProviderBase, ProviderResponse};
use kerness::session::SessionConfig;
use kerness::tooling::{ToolCall, ToolSpec};
use kerness::toolschema::ToolDialect;

/// One request a provider double received.
#[derive(Clone, Debug)]
pub struct Call {
    pub model: String,
    pub messages: Vec<Value>,
    /// What the session said the call was for: `orchestrator turn`,
    /// `orchestrator retry`, `turn from Alice`, `final summary`, `compaction`,
    /// or one of those with ` (tool followup)` appended.
    pub purpose: String,
    /// Names of the tool specs offered natively, empty under the text dialect.
    pub tools: Vec<String>,
}

impl Call {
    /// The system prompt this call carried, or `""` if it had none.
    pub fn system(&self) -> String {
        self.messages
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Every message body, joined — for asking whether *anything* said a thing.
    pub fn text(&self) -> String {
        self.messages
            .iter()
            .filter_map(|m| m.get("content"))
            .map(render_content)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The last message, which is the one the turn is answering.
    pub fn last(&self) -> Value {
        self.messages.last().cloned().unwrap_or(Value::Null)
    }
}

/// Anthropic puts content in a block list; OpenAI and the text dialect use a
/// string. Both flatten to something a test can search.
fn render_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| block.to_string())
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// A provider whose replies are written in advance, keyed by purpose.
///
/// A purpose map alone would answer every orchestrator turn identically and
/// never reach a terminator, so each key owns a *sequence* with its own cursor.
/// The last entry repeats once the sequence runs out, which is what lets a test
/// script the first three turns and let the rest hold steady.
pub struct ScriptedProvider {
    base: ProviderBase,
    name: String,
    dialect: ToolDialect,
    /// Purpose substring in declaration order, so an earlier `on` wins.
    scripts: Vec<(String, Vec<String>)>,
    fallback: Vec<String>,
    cursors: Mutex<HashMap<String, usize>>,
    calls: Mutex<Vec<Call>>,
}

impl ScriptedProvider {
    pub fn new() -> Self {
        ScriptedProvider {
            // Zero extra attempts: a test that scripts one reply should see one
            // call, and a failure should surface rather than being slept over.
            base: ProviderBase::new(0, 0.0, None),
            name: "scripted".to_string(),
            dialect: ToolDialect::Text,
            scripts: Vec::new(),
            fallback: Vec::new(),
            cursors: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Answer calls whose purpose contains *purpose* from *replies*, in order.
    pub fn on(mut self, purpose: &str, replies: &[&str]) -> Self {
        self.scripts.push((
            purpose.to_string(),
            replies.iter().map(|r| r.to_string()).collect(),
        ));
        self
    }

    /// Answer anything unmatched from *replies*, in order.
    pub fn fallback(mut self, replies: &[&str]) -> Self {
        self.fallback = replies.iter().map(|r| r.to_string()).collect();
        self
    }

    pub fn named(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    pub fn speaking(mut self, dialect: ToolDialect) -> Self {
        self.dialect = dialect;
        self
    }

    pub fn shared(self) -> Arc<ScriptedProvider> {
        Arc::new(self)
    }

    pub fn calls(&self) -> MutexGuard<'_, Vec<Call>> {
        self.calls.lock().expect("calls lock")
    }

    /// Purposes seen, in order — the cheapest way to assert a turn sequence.
    pub fn purposes(&self) -> Vec<String> {
        self.calls().iter().map(|c| c.purpose.clone()).collect()
    }

    pub fn call_count(&self) -> usize {
        self.calls().len()
    }

    /// The last call whose purpose contains *needle*.
    pub fn last_call_for(&self, needle: &str) -> Option<Call> {
        self.calls()
            .iter()
            .rev()
            .find(|c| c.purpose.contains(needle))
            .cloned()
    }

    fn reply_for(&self, purpose: &str) -> String {
        let mut cursors = self.cursors.lock().expect("cursor lock");
        for (key, replies) in &self.scripts {
            if !purpose.contains(key.as_str()) || replies.is_empty() {
                continue;
            }
            return take(&mut cursors, key, replies);
        }
        if self.fallback.is_empty() {
            return format!("(scripted provider has no reply for {purpose})");
        }
        take(&mut cursors, "", &self.fallback)
    }

    fn record(&self, model: &str, messages: &[Value], purpose: &str, tools: Option<&[ToolSpec]>) {
        self.calls().push(Call {
            model: model.to_string(),
            messages: messages.to_vec(),
            purpose: purpose.to_string(),
            tools: tool_names(tools),
        });
    }
}

impl Default for ScriptedProvider {
    fn default() -> Self {
        ScriptedProvider::new()
    }
}

/// Read one entry and advance, holding on the last so a sequence never runs dry.
fn take(cursors: &mut HashMap<String, usize>, key: &str, replies: &[String]) -> String {
    let cursor = cursors.entry(key.to_string()).or_insert(0);
    let index = (*cursor).min(replies.len() - 1);
    *cursor += 1;
    replies[index].clone()
}

fn tool_names(tools: Option<&[ToolSpec]>) -> Vec<String> {
    tools
        .unwrap_or(&[])
        .iter()
        .map(|spec| spec.name.clone())
        .collect()
}

impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base(&self) -> &ProviderBase {
        &self.base
    }

    fn tool_dialect(&self) -> ToolDialect {
        self.dialect
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        self.record(model, messages, "", tools);
        Ok(ProviderResponse::text(self.reply_for("")))
    }

    /// Overridden rather than left to the supplied body because the purpose is
    /// the thing being scripted on, and `chat` never sees it.
    fn chat_with_retries(
        &self,
        model: &str,
        messages: &[Value],
        purpose: &str,
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        self.record(model, messages, purpose, tools);
        Ok(ProviderResponse::text(self.reply_for(purpose)))
    }
}

/// A provider that returns whole responses, so a test can put tool calls in one.
///
/// Separate from [`ScriptedProvider`] because a tool-calling reply is not a
/// string: it carries `tool_calls`, and under the native dialects it carries
/// them *instead of* text.
pub struct ToolProvider {
    base: ProviderBase,
    dialect: ToolDialect,
    replies: Vec<ProviderResponse>,
    cursor: AtomicUsize,
    calls: Mutex<Vec<Call>>,
}

impl ToolProvider {
    pub fn new(dialect: ToolDialect, replies: Vec<ProviderResponse>) -> Self {
        ToolProvider {
            base: ProviderBase::new(0, 0.0, None),
            dialect,
            replies,
            cursor: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn shared(self) -> Arc<ToolProvider> {
        Arc::new(self)
    }

    pub fn calls(&self) -> MutexGuard<'_, Vec<Call>> {
        self.calls.lock().expect("calls lock")
    }

    pub fn call_count(&self) -> usize {
        self.calls().len()
    }
}

impl Provider for ToolProvider {
    fn name(&self) -> &str {
        "tool-provider"
    }

    fn base(&self) -> &ProviderBase {
        &self.base
    }

    fn tool_dialect(&self) -> ToolDialect {
        self.dialect
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        self.calls().push(Call {
            model: model.to_string(),
            messages: messages.to_vec(),
            purpose: String::new(),
            tools: tool_names(tools),
        });
        let index = self
            .cursor
            .fetch_add(1, Ordering::SeqCst)
            .min(self.replies.len().saturating_sub(1));
        Ok(self
            .replies
            .get(index)
            .cloned()
            .unwrap_or_else(|| ProviderResponse::text("(no replies configured)")))
    }
}

/// A reply that asks for one tool call, in whichever shape the dialect uses.
pub fn tool_call_reply(name: &str, arguments: Value, id: &str) -> ProviderResponse {
    ProviderResponse {
        tool_calls: vec![ToolCall {
            name: name.to_string(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
            id: id.to_string(),
        }],
        ..ProviderResponse::default()
    }
}

/// The same request written as a fenced block, for the text dialect.
pub fn fenced_tool_call(name: &str, arguments: Value) -> String {
    let payload = json!({"tool_calls": [{"name": name, "arguments": arguments}]});
    format!(
        "```tool_calls\n{}\n```",
        serde_json::to_string_pretty(&payload).expect("static value")
    )
}

/// A channel that keeps what it was handed instead of printing it.
#[derive(Default)]
pub struct RecordingChannel {
    sent: Mutex<Vec<(String, String)>>,
    system: Mutex<Vec<String>>,
}

impl RecordingChannel {
    pub fn new() -> Arc<RecordingChannel> {
        Arc::new(RecordingChannel::default())
    }

    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().expect("sent lock").clone()
    }

    pub fn senders(&self) -> Vec<String> {
        self.sent().into_iter().map(|(who, _)| who).collect()
    }

    pub fn system(&self) -> Vec<String> {
        self.system.lock().expect("system lock").clone()
    }

    /// Whether any system notice contains *needle*.
    pub fn noted(&self, needle: &str) -> bool {
        self.system().iter().any(|line| line.contains(needle))
    }
}

impl Channel for RecordingChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        self.sent
            .lock()
            .expect("sent lock")
            .push((sender.to_string(), message.to_string()));
        Ok(())
    }

    fn send_system(&self, message: &str) -> Result<()> {
        self.system
            .lock()
            .expect("system lock")
            .push(message.to_string());
        Ok(())
    }

    fn type_name(&self) -> String {
        "RecordingChannel".to_string()
    }
}

/// A directory that removes itself.
///
/// Hand-rolled rather than pulled in as a dev-dependency: the crate's whole
/// claim is that it needs nothing beyond what `Cargo.toml` already lists, and
/// this is twenty lines. Cargo gives each test binary its own process, so the
/// pid and a counter are enough to keep two of them apart.
pub struct TempDir {
    path: PathBuf,
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let unique = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "kerness-test-{}-{}-{}",
            std::process::id(),
            tag,
            unique
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Path as a `String`, which is what `SessionConfig` fields take.
    pub fn str_join(&self, name: &str) -> String {
        self.join(name).to_string_lossy().into_owned()
    }

    /// Write *contents* to *name*, creating parent directories, and return it.
    pub fn write(&self, name: &str, contents: &str) -> PathBuf {
        let target = self.join(name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&target, contents).expect("write fixture");
        target
    }

    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.join(name)).unwrap_or_default()
    }

    pub fn exists(&self, name: &str) -> bool {
        self.join(name).exists()
    }

    /// Sorted entry names, for asserting what a run left behind.
    pub fn entries(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.path) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The message from a call that had to fail.
///
/// `Result::expect_err` needs the *success* type to implement `Debug`, and
/// several of the framework's own do not — `Session` holds a provider and a
/// channel behind trait objects, neither of which has a debug form worth
/// deriving. Asking for the error directly avoids putting that bound on the
/// public API to satisfy a test.
pub fn refusal<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal, got success"),
        Err(error) => error.to_string(),
    }
}

/// A session configuration that runs at full speed and writes nothing.
///
/// `turn_delay` is the one field every test must override: the default second
/// between turns is right for a human watching a debate and wrong for a suite.
pub fn config(gameplan: &str, topic: &str, provider: Arc<dyn Provider>) -> SessionConfig {
    SessionConfig {
        gameplan: gameplan.to_string(),
        topic: topic.to_string(),
        provider: Some(provider),
        turn_delay: Duration::ZERO,
        ..Default::default()
    }
}
