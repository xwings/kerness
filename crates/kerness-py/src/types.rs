//! The value types every other module passes around.
//!
//! Each one wraps the framework struct it stands for and exposes the same
//! attribute names Python callers use. The dialect enum is the exception: it
//! is a `str`-backed `enum.Enum` written in Python, because callers compare it
//! with `is` and only a real enum member satisfies that.

use std::sync::{Arc, OnceLock};

use kerness::agent::Role;
use kerness::gameplan::GameplanConfig;
use kerness::harness::{
    AgentsSpec, HarnessSpec, LoopSpec, OrchestratorSpec, ParticipantSpec, PhaseSpec, ResultField,
    ResultType,
};
use kerness::persona::PersonaConfig;
use kerness::provider::ProviderResponse;
use kerness::pyfmt::repr_str;
use kerness::session::Memories;
use kerness::skill::loader::SkillConfig;
use kerness::tooling::{Arguments, ToolCall, ToolHandler, ToolSpec};
use kerness::toolkit::ToolResult;
use kerness::toolschema::ToolDialect;
use kerness::{Agent, Memory, Message, Turn};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple, PyType};
use serde_json::Value;

use crate::convert::{map_from_py, map_to_py, optional_map, value_from_py, value_to_py};
use crate::errors::{to_py, Catch, Raise};
use crate::provider::bind_provider;

// ---------------------------------------------------------------- ToolDialect

static DIALECT: OnceLock<Py<PyAny>> = OnceLock::new();

/// Record the `ToolDialect` enum from an already-imported `kerness._enums`.
pub fn register_dialect(class: &Bound<'_, PyAny>) {
    let _ = DIALECT.set(class.clone().unbind());
}

/// The enum member standing for *dialect*.
pub fn dialect_to_py<'py>(py: Python<'py>, dialect: ToolDialect) -> PyResult<Bound<'py, PyAny>> {
    DIALECT
        .get()
        .expect("kerness._enums has not been registered with kerness._core")
        .bind(py)
        .call1((dialect.as_str(),))
}

/// The dialect an enum member — or its bare value — stands for.
pub fn dialect_from_py(object: &Bound<'_, PyAny>) -> PyResult<ToolDialect> {
    let text = match object.getattr("value") {
        Ok(value) => value.str()?.to_string_lossy().into_owned(),
        Err(_) => object.str()?.to_string_lossy().into_owned(),
    };
    ToolDialect::parse(&text)
        .ok_or_else(|| to_py(kerness::Error::Value(format!("Unknown tool dialect {text}"))))
}

// ------------------------------------------------------------------- ToolCall

/// A call the model asked for.
#[pyclass(name = "ToolCall", module = "kerness._core", frozen)]
#[derive(Clone)]
pub struct PyToolCall {
    pub inner: ToolCall,
}

#[pymethods]
impl PyToolCall {
    #[new]
    #[pyo3(signature = (name, arguments=None, id=String::new()))]
    fn new(name: String, arguments: Option<&Bound<'_, PyAny>>, id: String) -> PyResult<Self> {
        Ok(PyToolCall {
            inner: ToolCall {
                name,
                arguments: optional_map(arguments)?,
                id,
            },
        })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn arguments<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.arguments)
    }

    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyToolCall>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "ToolCall(name={}, arguments={}, id={})",
            repr_str(&self.inner.name),
            kerness::pyfmt::repr(&Value::Object(self.inner.arguments.clone())),
            repr_str(&self.inner.id),
        )
    }
}

// ------------------------------------------------------------------- ToolSpec

/// A Python callable used as a tool handler.
struct PyHandler {
    callable: Py<PyAny>,
    takes_actor: bool,
}

impl ToolHandler for PyHandler {
    fn call(&self, arguments: &Arguments, actor: &str) -> kerness::Result<String> {
        Python::with_gil(|py| {
            let arguments = map_to_py(py, arguments)?;
            let callable = self.callable.bind(py);
            let value = if self.takes_actor {
                callable.call1((arguments, actor))?
            } else {
                callable.call1((arguments,))?
            };
            stringify(&value)
        })
        .catch()
    }
}

/// Render a handler's return value as text for the model.
///
/// A list becomes a comma-joined string, which is what `list_dir` produces.
fn stringify(value: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(text) = value.downcast::<PyString>() {
        return Ok(text.to_str()?.to_owned());
    }
    if value.downcast::<PyList>().is_ok() || value.downcast::<PyTuple>().is_ok() {
        let mut parts = Vec::new();
        for item in value.try_iter()? {
            parts.push(item?.str()?.to_string_lossy().into_owned());
        }
        return Ok(parts.join(", "));
    }
    Ok(value.str()?.to_string_lossy().into_owned())
}

/// A tool an agent may call.
#[pyclass(name = "ToolSpec", module = "kerness._core", frozen)]
pub struct PyToolSpec {
    pub inner: ToolSpec,
    /// Kept so `spec.handler` hands back the object the caller registered.
    handler: Py<PyAny>,
}

impl Clone for PyToolSpec {
    fn clone(&self) -> Self {
        Python::with_gil(|py| PyToolSpec {
            inner: self.inner.clone(),
            handler: self.handler.clone_ref(py),
        })
    }
}

impl PyToolSpec {
    /// Wrap a framework spec whose handler is not a Python object.
    pub fn adopt(py: Python<'_>, inner: ToolSpec) -> Self {
        PyToolSpec {
            handler: py.None(),
            inner,
        }
    }
}

#[pymethods]
impl PyToolSpec {
    #[new]
    #[pyo3(signature = (name, description, parameters, handler, takes_actor=false))]
    fn new(
        name: String,
        description: String,
        parameters: &Bound<'_, PyAny>,
        handler: Py<PyAny>,
        takes_actor: bool,
    ) -> PyResult<Self> {
        let py = parameters.py();
        let mut inner = ToolSpec::new(
            name,
            description,
            value_from_py(parameters)?,
            Arc::new(PyHandler {
                callable: handler.clone_ref(py),
                takes_actor,
            }),
        );
        inner.takes_actor = takes_actor;
        Ok(PyToolSpec { inner, handler })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[getter]
    fn parameters<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.inner.parameters)
    }

    #[getter]
    fn handler(&self, py: Python<'_>) -> Py<PyAny> {
        self.handler.clone_ref(py)
    }

    #[getter]
    fn takes_actor(&self) -> bool {
        self.inner.takes_actor
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyToolSpec>().is_ok_and(|other| {
            other.inner.name == self.inner.name
                && other.inner.description == self.inner.description
                && other.inner.parameters == self.inner.parameters
                && other.inner.takes_actor == self.inner.takes_actor
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "ToolSpec(name={}, takes_actor={})",
            repr_str(&self.inner.name),
            pybool(self.inner.takes_actor),
        )
    }
}

/// How Python spells a boolean, for the `__repr__` implementations.
pub fn pybool(flag: bool) -> &'static str {
    if flag {
        "True"
    } else {
        "False"
    }
}

// ----------------------------------------------------------------- ToolResult

/// What a tool call produced.
#[pyclass(name = "ToolResult", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyToolResult {
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

impl PyToolResult {
    pub fn adopt(result: ToolResult) -> Self {
        PyToolResult {
            name: result.name,
            content: result.content,
            is_error: result.is_error,
        }
    }

    pub fn snapshot(&self) -> ToolResult {
        ToolResult {
            name: self.name.clone(),
            content: self.content.clone(),
            is_error: self.is_error,
        }
    }
}

#[pymethods]
impl PyToolResult {
    #[new]
    #[pyo3(signature = (name, content, is_error=false))]
    fn new(name: String, content: String, is_error: bool) -> Self {
        PyToolResult {
            name,
            content,
            is_error,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyToolResult>().is_ok_and(|other| {
            other.name == self.name
                && other.content == self.content
                && other.is_error == self.is_error
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "ToolResult(name={}, content={}, is_error={})",
            repr_str(&self.name),
            repr_str(&self.content),
            pybool(self.is_error),
        )
    }
}

// ----------------------------------------------------------- ProviderResponse

/// One reply from a provider, normalized across backends.
#[pyclass(name = "ProviderResponse", module = "kerness._core")]
pub struct PyProviderResponse {
    pub inner: ProviderResponse,
    /// The decoded payload as the caller's own object, which for a pydantic
    /// model is the validated instance rather than a plain dict.
    pub structured: Option<Py<PyAny>>,
}

impl Clone for PyProviderResponse {
    fn clone(&self) -> Self {
        Python::with_gil(|py| PyProviderResponse {
            inner: self.inner.clone(),
            structured: self.structured.as_ref().map(|object| object.clone_ref(py)),
        })
    }
}

impl PyProviderResponse {
    pub fn adopt(inner: ProviderResponse) -> Self {
        PyProviderResponse {
            inner,
            structured: None,
        }
    }
}

#[pymethods]
impl PyProviderResponse {
    #[new]
    #[pyo3(signature = (
        content,
        model=String::new(),
        usage=None,
        raw=None,
        structured=None,
        tool_calls=None,
        stop_reason=String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        content: String,
        model: String,
        usage: Option<&Bound<'_, PyAny>>,
        raw: Option<&Bound<'_, PyAny>>,
        structured: Option<Bound<'_, PyAny>>,
        tool_calls: Option<Vec<PyToolCall>>,
        stop_reason: String,
    ) -> PyResult<Self> {
        Ok(PyProviderResponse {
            inner: ProviderResponse {
                content,
                model,
                usage: optional_map(usage)?,
                raw: match raw {
                    Some(raw) if !raw.is_none() => value_from_py(raw)?,
                    _ => Value::Object(serde_json::Map::new()),
                },
                structured: None,
                tool_calls: tool_calls
                    .unwrap_or_default()
                    .into_iter()
                    .map(|call| call.inner)
                    .collect(),
                stop_reason,
            },
            structured: structured
                .filter(|object| !object.is_none())
                .map(Bound::unbind),
        })
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[getter]
    fn model(&self) -> &str {
        &self.inner.model
    }

    #[getter]
    fn usage<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.usage)
    }

    #[getter]
    fn raw<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.inner.raw)
    }

    #[getter]
    fn structured<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match (&self.structured, &self.inner.structured) {
            (Some(object), _) => Ok(object.bind(py).clone()),
            (None, Some(value)) => value_to_py(py, value),
            (None, None) => Ok(py.None().into_bound(py)),
        }
    }

    #[setter]
    fn set_structured(&mut self, value: Option<Bound<'_, PyAny>>) {
        self.structured = value
            .filter(|object| !object.is_none())
            .map(Bound::unbind);
        self.inner.structured = None;
    }

    #[getter]
    fn tool_calls(&self) -> Vec<PyToolCall> {
        self.inner
            .tool_calls
            .iter()
            .map(|call| PyToolCall {
                inner: call.clone(),
            })
            .collect()
    }

    #[getter]
    fn stop_reason(&self) -> &str {
        &self.inner.stop_reason
    }

    fn __repr__(&self) -> String {
        format!(
            "ProviderResponse(content={}, model={})",
            repr_str(&self.inner.content),
            repr_str(&self.inner.model),
        )
    }
}

// -------------------------------------------------------------- Message, Turn

/// One entry in the public transcript.
#[pyclass(name = "Message", module = "kerness._core")]
#[derive(Clone)]
pub struct PyMessage {
    pub inner: Message,
}

#[pymethods]
impl PyMessage {
    #[new]
    #[pyo3(signature = (sender, content, round_idx=0, msg_type="turn".to_string()))]
    fn new(sender: String, content: String, round_idx: i64, msg_type: String) -> Self {
        PyMessage {
            inner: Message {
                sender,
                content,
                round_idx,
                msg_type,
            },
        }
    }

    #[getter]
    fn sender(&self) -> &str {
        &self.inner.sender
    }

    #[setter]
    fn set_sender(&mut self, value: String) {
        self.inner.sender = value;
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[setter]
    fn set_content(&mut self, value: String) {
        self.inner.content = value;
    }

    #[getter]
    fn round_idx(&self) -> i64 {
        self.inner.round_idx
    }

    #[setter]
    fn set_round_idx(&mut self, value: i64) {
        self.inner.round_idx = value;
    }

    #[getter]
    fn msg_type(&self) -> &str {
        &self.inner.msg_type
    }

    #[setter]
    fn set_msg_type(&mut self, value: String) {
        self.inner.msg_type = value;
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyMessage>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "Message(sender={}, content={}, round_idx={}, msg_type={})",
            repr_str(&self.inner.sender),
            repr_str(&self.inner.content),
            self.inner.round_idx,
            repr_str(&self.inner.msg_type),
        )
    }
}

/// One entry in what the model is shown.
#[pyclass(name = "Turn", module = "kerness._core", frozen)]
#[derive(Clone)]
pub struct PyTurn {
    pub inner: Turn,
}

#[pymethods]
impl PyTurn {
    #[new]
    #[pyo3(signature = (role, speaker, content, round_idx=0, msg_type="turn".to_string()))]
    fn new(
        role: String,
        speaker: String,
        content: String,
        round_idx: i64,
        msg_type: String,
    ) -> Self {
        PyTurn {
            inner: Turn {
                role,
                speaker,
                content,
                round_idx,
                msg_type,
            },
        }
    }

    #[getter]
    fn role(&self) -> &str {
        &self.inner.role
    }

    #[getter]
    fn speaker(&self) -> &str {
        &self.inner.speaker
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[getter]
    fn round_idx(&self) -> i64 {
        self.inner.round_idx
    }

    #[getter]
    fn msg_type(&self) -> &str {
        &self.inner.msg_type
    }

    /// The chat message this turn renders to.
    fn render<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let rendered = self.inner.render();
        let dict = PyDict::new(py);
        dict.set_item("role", rendered.role)?;
        dict.set_item("content", rendered.content)?;
        Ok(dict)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyTurn>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "Turn(role={}, speaker={}, content={}, round_idx={}, msg_type={})",
            repr_str(&self.inner.role),
            repr_str(&self.inner.speaker),
            repr_str(&self.inner.content),
            self.inner.round_idx,
            repr_str(&self.inner.msg_type),
        )
    }
}

// ---------------------------------------------------------------------- Agent

/// An LLM-backed participant in a session.
#[pyclass(name = "Agent", module = "kerness._core")]
pub struct PyAgent {
    pub inner: Agent,
    /// The provider object the caller supplied, kept so the attribute reads
    /// back as what was set rather than as an opaque wrapper.
    pub provider: Option<Py<PyAny>>,
}

impl PyAgent {
    /// A clone of the framework agent, which is what a session stores.
    pub fn snapshot(&self) -> Agent {
        self.inner.clone()
    }
}

#[pymethods]
impl PyAgent {
    #[new]
    #[pyo3(signature = (
        name,
        model=String::new(),
        persona=String::new(),
        role="participant".to_string(),
        language=String::new(),
        system_prompt=String::new(),
        provider=None,
        skills=None,
        memory=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: String,
        model: String,
        persona: String,
        role: String,
        language: String,
        system_prompt: String,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        memory: Option<String>,
    ) -> PyResult<Self> {
        let provider = provider.filter(|object| !object.is_none());
        Ok(PyAgent {
            inner: Agent {
                name,
                model,
                persona,
                role: Role::parse(&role).raise()?,
                language,
                system_prompt,
                provider: match &provider {
                    Some(object) => bind_provider(object)?,
                    None => None,
                },
                skills,
                memory,
            },
            provider: provider.map(Bound::unbind),
        })
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[setter]
    fn set_name(&mut self, value: String) {
        self.inner.name = value;
    }

    #[getter]
    fn model(&self) -> &str {
        &self.inner.model
    }

    #[setter]
    fn set_model(&mut self, value: String) {
        self.inner.model = value;
    }

    #[getter]
    fn persona(&self) -> &str {
        &self.inner.persona
    }

    #[setter]
    fn set_persona(&mut self, value: String) {
        self.inner.persona = value;
    }

    #[getter]
    fn role(&self) -> &str {
        self.inner.role.as_str()
    }

    #[setter]
    fn set_role(&mut self, value: String) -> PyResult<()> {
        self.inner.role = Role::parse(&value).raise()?;
        Ok(())
    }

    #[getter]
    fn language(&self) -> &str {
        &self.inner.language
    }

    #[setter]
    fn set_language(&mut self, value: String) {
        self.inner.language = value;
    }

    #[getter]
    fn system_prompt(&self) -> &str {
        &self.inner.system_prompt
    }

    #[setter]
    fn set_system_prompt(&mut self, value: String) {
        self.inner.system_prompt = value;
    }

    #[getter]
    fn provider(&self, py: Python<'_>) -> Py<PyAny> {
        match &self.provider {
            Some(object) => object.clone_ref(py),
            None => py.None(),
        }
    }

    #[setter]
    fn set_provider(&mut self, value: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        let value = value.filter(|object| !object.is_none());
        self.inner.provider = match &value {
            Some(object) => bind_provider(object)?,
            None => None,
        };
        self.provider = value.map(Bound::unbind);
        Ok(())
    }

    #[getter]
    fn skills(&self) -> Option<Vec<String>> {
        self.inner.skills.clone()
    }

    #[setter]
    fn set_skills(&mut self, value: Option<Vec<String>>) {
        self.inner.skills = value;
    }

    #[getter]
    fn memory(&self) -> Option<String> {
        self.inner.memory.clone()
    }

    #[setter]
    fn set_memory(&mut self, value: Option<String>) {
        self.inner.memory = value;
    }

    /// Whether this agent conducts the session.
    #[getter]
    fn is_orchestrator(&self) -> bool {
        self.inner.is_orchestrator()
    }

    /// Whether this agent speaks in the rounds.
    #[getter]
    fn is_participant(&self) -> bool {
        self.inner.is_participant()
    }

    /// Build the full system prompt, falling back to *default_prompt*.
    #[pyo3(signature = (default_prompt="", show_reasoning=None, skills_prompt=""))]
    fn build_system_prompt(
        &self,
        default_prompt: &str,
        show_reasoning: Option<bool>,
        skills_prompt: &str,
    ) -> PyResult<String> {
        self.inner
            .build_system_prompt(default_prompt, show_reasoning, skills_prompt)
            .raise()
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        let Ok(other) = other.downcast::<PyAgent>() else {
            return false;
        };
        let other = other.borrow();
        let (left, right) = (&self.inner, &other.inner);
        left.name == right.name
            && left.model == right.model
            && left.persona == right.persona
            && left.role == right.role
            && left.language == right.language
            && left.system_prompt == right.system_prompt
            && left.skills == right.skills
            && left.memory == right.memory
    }

    fn __repr__(&self) -> String {
        format!(
            "Agent(name={}, model={}, persona={}, role={}, language={}, system_prompt={})",
            repr_str(&self.inner.name),
            repr_str(&self.inner.model),
            repr_str(&self.inner.persona),
            repr_str(self.inner.role.as_str()),
            repr_str(&self.inner.language),
            repr_str(&self.inner.system_prompt),
        )
    }
}

// --------------------------------------------------------------------- Memory

/// Where a [`PyMemory`] keeps its file.
///
/// A memory a caller built stands alone. A session's does not: the session
/// writes to it during the run, so `session.memory.read()` afterwards has to
/// reach the session's own file and not a copy taken when the attribute was
/// first read.
enum Store {
    Owned(Memory),
    Session(Arc<std::sync::Mutex<Memories>>),
}

impl Store {
    /// Run *act* against the file this memory stands for.
    fn with<T>(&mut self, act: impl FnOnce(&mut Memory) -> T) -> T {
        match self {
            Store::Owned(memory) => act(memory),
            Store::Session(memories) => act(&mut lock(memories).session),
        }
    }
}

/// Take a session lock, treating poisoning as the panic it reports.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A file-backed note the session can read and, when allowed, append to.
#[pyclass(name = "Memory", module = "kerness._core")]
pub struct PyMemory {
    store: Store,
}

impl PyMemory {
    /// A handle on the session-level memory of a running session.
    pub fn of_session(memories: Arc<std::sync::Mutex<Memories>>) -> Self {
        PyMemory {
            store: Store::Session(memories),
        }
    }
}

#[pymethods]
impl PyMemory {
    #[new]
    #[pyo3(signature = (path="memory.md".to_string()))]
    fn new(path: String) -> Self {
        PyMemory {
            store: Store::Owned(Memory::new(path)),
        }
    }

    #[getter]
    fn path(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let path = self.store.with(|memory| memory.path().to_path_buf());
        path_to_py(py, path.to_string_lossy().as_ref())
    }

    /// Read the file into memory, treating an absent file as empty.
    fn load(&mut self) -> PyResult<()> {
        self.store.with(Memory::load).raise()
    }

    /// The loaded text.
    fn read(&mut self) -> String {
        self.store.with(|memory| memory.read().to_string())
    }

    /// Append *text* verbatim.
    fn append(&mut self, text: &str) -> PyResult<()> {
        self.store.with(|memory| memory.append(text)).raise()
    }

    /// Append *text* as its own block.
    fn append_entry(&mut self, text: &str) -> PyResult<()> {
        self.store.with(|memory| memory.append_entry(text)).raise()
    }

    /// Replace the file's contents.
    fn write(&mut self, content: &str) -> PyResult<()> {
        self.store.with(|memory| memory.write(content)).raise()
    }
}

// -------------------------------------------------------------- PersonaConfig

/// A persona file's four sections.
#[pyclass(name = "PersonaConfig", module = "kerness._core", get_all, set_all)]
#[derive(Clone)]
pub struct PyPersonaConfig {
    pub name: String,
    pub persona: String,
    pub background: String,
    pub communication_style: String,
}

impl PyPersonaConfig {
    pub fn adopt(config: PersonaConfig) -> Self {
        PyPersonaConfig {
            name: config.name,
            persona: config.persona,
            background: config.background,
            communication_style: config.communication_style,
        }
    }

    pub fn snapshot(&self) -> PersonaConfig {
        PersonaConfig {
            name: self.name.clone(),
            persona: self.persona.clone(),
            background: self.background.clone(),
            communication_style: self.communication_style.clone(),
        }
    }
}

#[pymethods]
impl PyPersonaConfig {
    #[new]
    #[pyo3(signature = (
        name=String::new(),
        persona=String::new(),
        background=String::new(),
        communication_style=String::new(),
    ))]
    fn new(name: String, persona: String, background: String, communication_style: String) -> Self {
        PyPersonaConfig {
            name,
            persona,
            background,
            communication_style,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyPersonaConfig>().is_ok_and(|other| {
            other.name == self.name
                && other.persona == self.persona
                && other.background == self.background
                && other.communication_style == self.communication_style
        })
    }

    fn __repr__(&self) -> String {
        format!("PersonaConfig(name={})", repr_str(&self.name))
    }
}

// ---------------------------------------------------------------- SkillConfig

/// One `SKILL.md` file.
#[pyclass(name = "SkillConfig", module = "kerness._core")]
#[derive(Clone)]
pub struct PySkillConfig {
    pub inner: SkillConfig,
}

#[pymethods]
impl PySkillConfig {
    #[new]
    #[pyo3(signature = (
        name,
        description=String::new(),
        content=String::new(),
        allowed_tools=None,
        base_dir=None,
        builtin=false,
    ))]
    fn new(
        name: String,
        description: String,
        content: String,
        allowed_tools: Option<Vec<String>>,
        base_dir: Option<String>,
        builtin: bool,
    ) -> Self {
        PySkillConfig {
            inner: SkillConfig {
                name,
                description,
                content,
                allowed_tools,
                base_dir: base_dir.map(Into::into),
                builtin,
            },
        }
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[setter]
    fn set_name(&mut self, value: String) {
        self.inner.name = value;
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[setter]
    fn set_description(&mut self, value: String) {
        self.inner.description = value;
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[setter]
    fn set_content(&mut self, value: String) {
        self.inner.content = value;
    }

    #[getter]
    fn allowed_tools(&self) -> Option<Vec<String>> {
        self.inner.allowed_tools.clone()
    }

    #[setter]
    fn set_allowed_tools(&mut self, value: Option<Vec<String>>) {
        self.inner.allowed_tools = value;
    }

    #[getter]
    fn base_dir(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match &self.inner.base_dir {
            None => Ok(py.None()),
            Some(path) => path_to_py(py, path.to_string_lossy().as_ref()),
        }
    }

    #[setter]
    fn set_base_dir(&mut self, value: Option<String>) {
        self.inner.base_dir = value.map(Into::into);
    }

    #[getter]
    fn builtin(&self) -> bool {
        self.inner.builtin
    }

    #[setter]
    fn set_builtin(&mut self, value: bool) {
        self.inner.builtin = value;
    }

    /// The bundled directories this skill grants read access to.
    fn bundle_paths(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .bundle_paths()
            .iter()
            .map(|path| path_to_py(py, path.to_string_lossy().as_ref()))
            .collect()
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PySkillConfig>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "SkillConfig(name={}, builtin={})",
            repr_str(&self.inner.name),
            pybool(self.inner.builtin),
        )
    }
}

/// A filesystem path as `pathlib.Path`, which is what callers compare against.
pub fn path_to_py(py: Python<'_>, path: &str) -> PyResult<Py<PyAny>> {
    Ok(py
        .import("pathlib")?
        .getattr("Path")?
        .call1((path,))?
        .unbind())
}

// -------------------------------------------------------------- harness specs

/// Whether the harness demands an orchestrator, and what to tell it.
#[pyclass(name = "OrchestratorSpec", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyOrchestratorSpec {
    pub required: bool,
    pub instruction: String,
}

#[pymethods]
impl PyOrchestratorSpec {
    #[new]
    #[pyo3(signature = (required=false, instruction=String::new()))]
    fn new(required: bool, instruction: String) -> Self {
        PyOrchestratorSpec {
            required,
            instruction,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyOrchestratorSpec>().is_ok_and(|other| {
            other.required == self.required && other.instruction == self.instruction
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "OrchestratorSpec(required={}, instruction={})",
            pybool(self.required),
            repr_str(&self.instruction),
        )
    }
}

/// How many participants the harness accepts.
#[pyclass(name = "ParticipantSpec", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyParticipantSpec {
    pub min: i64,
    pub max: Option<i64>,
}

#[pymethods]
impl PyParticipantSpec {
    #[new]
    #[pyo3(signature = (min=1, max=None))]
    fn new(min: i64, max: Option<i64>) -> Self {
        PyParticipantSpec { min, max }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyParticipantSpec>()
            .is_ok_and(|other| other.min == self.min && other.max == self.max)
    }

    fn __repr__(&self) -> String {
        match self.max {
            Some(max) => format!("ParticipantSpec(min={}, max={max})", self.min),
            None => format!("ParticipantSpec(min={}, max=None)", self.min),
        }
    }
}

/// The cast the harness expects.
#[pyclass(name = "AgentsSpec", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyAgentsSpec {
    pub orchestrator: PyOrchestratorSpec,
    pub participants: PyParticipantSpec,
}

#[pymethods]
impl PyAgentsSpec {
    #[new]
    #[pyo3(signature = (orchestrator=None, participants=None))]
    fn new(orchestrator: Option<PyOrchestratorSpec>, participants: Option<PyParticipantSpec>) -> Self {
        PyAgentsSpec {
            orchestrator: orchestrator.unwrap_or(PyOrchestratorSpec {
                required: false,
                instruction: String::new(),
            }),
            participants: participants.unwrap_or(PyParticipantSpec { min: 1, max: None }),
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyAgentsSpec>().is_ok_and(|other| {
            other.orchestrator.required == self.orchestrator.required
                && other.orchestrator.instruction == self.orchestrator.instruction
                && other.participants.min == self.participants.min
                && other.participants.max == self.participants.max
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "AgentsSpec(orchestrator={}, participants={})",
            self.orchestrator.__repr__(),
            self.participants.__repr__(),
        )
    }
}

/// One named stage of the run.
#[pyclass(name = "PhaseSpec", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyPhaseSpec {
    pub name: String,
    pub instruction: String,
    pub rounds: i64,
    pub rethink: bool,
}

impl PyPhaseSpec {
    pub fn snapshot(&self) -> PhaseSpec {
        PhaseSpec {
            name: self.name.clone(),
            instruction: self.instruction.clone(),
            rounds: self.rounds,
            rethink: self.rethink,
        }
    }
}

#[pymethods]
impl PyPhaseSpec {
    #[new]
    #[pyo3(signature = (name=String::new(), instruction=String::new(), rounds=1, rethink=false))]
    fn new(name: String, instruction: String, rounds: i64, rethink: bool) -> Self {
        PyPhaseSpec {
            name,
            instruction,
            rounds,
            rethink,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyPhaseSpec>()
            .is_ok_and(|other| other.snapshot() == self.snapshot())
    }

    fn __repr__(&self) -> String {
        format!(
            "PhaseSpec(name={}, instruction={}, rounds={}, rethink={})",
            repr_str(&self.name),
            repr_str(&self.instruction),
            self.rounds,
            pybool(self.rethink),
        )
    }
}

/// The loop's limits, phases, and termination tokens.
#[pyclass(name = "LoopSpec", module = "kerness._core", frozen)]
#[derive(Clone)]
pub struct PyLoopSpec {
    pub inner: LoopSpec,
}

#[pymethods]
impl PyLoopSpec {
    #[new]
    #[pyo3(signature = (
        max_turns=50,
        max_rounds=3,
        terminate_on=None,
        phases=None,
        advance_on="NEXT_PHASE".to_string(),
        orchestrator_retries=2,
        verdict_rethink=true,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_turns: i64,
        max_rounds: i64,
        terminate_on: Option<Vec<String>>,
        phases: Option<Vec<PyPhaseSpec>>,
        advance_on: String,
        orchestrator_retries: i64,
        verdict_rethink: bool,
    ) -> Self {
        PyLoopSpec {
            inner: LoopSpec {
                max_turns,
                max_rounds,
                terminate_on: terminate_on
                    .unwrap_or_else(|| vec!["END_SESSION".to_string()]),
                phases: phases
                    .unwrap_or_default()
                    .iter()
                    .map(PyPhaseSpec::snapshot)
                    .collect(),
                advance_on,
                orchestrator_retries,
                verdict_rethink,
            },
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyLoopSpec>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "LoopSpec(max_turns={}, max_rounds={}, phases={})",
            self.inner.max_turns,
            self.inner.max_rounds,
            self.inner.phases.len(),
        )
    }

    #[getter]
    fn max_turns(&self) -> i64 {
        self.inner.max_turns
    }

    #[getter]
    fn max_rounds(&self) -> i64 {
        self.inner.max_rounds
    }

    #[getter]
    fn terminate_on<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.inner.terminate_on)
    }

    #[getter]
    fn phases<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.inner.phases.iter().map(phase_spec))
    }

    #[getter]
    fn advance_on(&self) -> &str {
        &self.inner.advance_on
    }

    #[getter]
    fn orchestrator_retries(&self) -> i64 {
        self.inner.orchestrator_retries
    }

    #[getter]
    fn verdict_rethink(&self) -> bool {
        self.inner.verdict_rethink
    }

    /// The termination token that means the cast agreed, if any.
    #[getter]
    fn consensus_keyword(&self) -> Option<&str> {
        self.inner.consensus_keyword()
    }
}

fn phase_spec(spec: &PhaseSpec) -> PyPhaseSpec {
    PyPhaseSpec {
        name: spec.name.clone(),
        instruction: spec.instruction.clone(),
        rounds: spec.rounds,
        rethink: spec.rethink,
    }
}

/// One field of the declared result shape.
#[pyclass(name = "ResultField", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyResultField {
    pub name: String,
    pub type_name: String,
    pub description: String,
}

impl PyResultField {
    pub fn adopt(field: &ResultField) -> Self {
        PyResultField {
            name: field.name.clone(),
            type_name: field.type_name.clone(),
            description: field.description.clone(),
        }
    }

    pub fn snapshot(&self) -> ResultField {
        ResultField {
            name: self.name.clone(),
            type_name: self.type_name.clone(),
            description: self.description.clone(),
        }
    }
}

#[pymethods]
impl PyResultField {
    #[new]
    #[pyo3(signature = (name, type_name="str".to_string(), description=String::new()))]
    fn new(name: String, type_name: String, description: String) -> Self {
        PyResultField {
            name,
            type_name,
            description,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyResultField>()
            .is_ok_and(|other| other.snapshot() == self.snapshot())
    }

    fn __repr__(&self) -> String {
        format!(
            "ResultField(name={}, type_name={}, description={})",
            repr_str(&self.name),
            repr_str(&self.type_name),
            repr_str(&self.description),
        )
    }

    /// The Python type this field's declared name stands for.
    #[getter]
    fn py_type<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyType>> {
        let name = match self.snapshot().result_type() {
            ResultType::Str => "str",
            ResultType::Int => "int",
            ResultType::Float => "float",
            ResultType::Bool => "bool",
            ResultType::List => "list",
            ResultType::Dict => "dict",
        };
        Ok(py.import("builtins")?.getattr(name)?.downcast_into()?)
    }
}

/// The machine-readable contract a gameplan declares.
#[pyclass(name = "HarnessSpec", module = "kerness._core", frozen)]
#[derive(Clone)]
pub struct PyHarnessSpec {
    pub inner: HarnessSpec,
}

#[pymethods]
impl PyHarnessSpec {
    // `loop` is a Python keyword-free name but a Rust keyword, so the argument
    // is spelled raw and reaches Python as `loop`.
    #[new]
    #[pyo3(signature = (
        name=String::new(),
        description=String::new(),
        agents=None,
        r#loop=None,
        tools=None,
        skills=None,
        result=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: String,
        description: String,
        agents: Option<PyAgentsSpec>,
        r#loop: Option<PyLoopSpec>,
        tools: Option<Vec<String>>,
        skills: Option<Vec<String>>,
        result: Option<Vec<PyResultField>>,
    ) -> Self {
        let agents = agents.unwrap_or_else(|| PyAgentsSpec::new(None, None));
        PyHarnessSpec {
            inner: HarnessSpec {
                name,
                description,
                agents: AgentsSpec {
                    orchestrator: OrchestratorSpec {
                        required: agents.orchestrator.required,
                        instruction: agents.orchestrator.instruction,
                    },
                    participants: ParticipantSpec {
                        min: agents.participants.min,
                        max: agents.participants.max,
                    },
                },
                loop_spec: r#loop.map(|spec| spec.inner).unwrap_or_default(),
                tools,
                skills,
                result: result
                    .unwrap_or_default()
                    .iter()
                    .map(PyResultField::snapshot)
                    .collect(),
            },
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PyHarnessSpec>()
            .is_ok_and(|other| other.inner == self.inner)
    }

    fn __repr__(&self) -> String {
        format!("HarnessSpec(name={})", repr_str(&self.inner.name))
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn description(&self) -> &str {
        &self.inner.description
    }

    #[getter]
    fn agents(&self) -> PyAgentsSpec {
        agents_spec(&self.inner.agents)
    }

    // `loop` is a Rust keyword, so the Python name is spelled out separately.
    #[getter]
    #[pyo3(name = "loop")]
    fn loop_spec(&self) -> PyLoopSpec {
        PyLoopSpec {
            inner: self.inner.loop_spec.clone(),
        }
    }

    #[getter]
    fn tools<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        optional_names(py, self.inner.tools.as_deref())
    }

    #[getter]
    fn skills<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        optional_names(py, self.inner.skills.as_deref())
    }

    #[getter]
    fn result<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            self.inner.result.iter().map(|field| PyResultField {
                name: field.name.clone(),
                type_name: field.type_name.clone(),
                description: field.description.clone(),
            }),
        )
    }

    /// Narrow *registered* to the names this harness permits.
    fn resolve_tools(&self, registered: Vec<String>) -> PyResult<Vec<String>> {
        self.inner.resolve_tools(&registered).raise()
    }

    /// Narrow *session_skills* to the names this harness permits.
    fn resolve_skills(&self, session_skills: Vec<String>) -> Vec<String> {
        self.inner.resolve_skills(&session_skills)
    }
}

/// A name list as a tuple, keeping "not declared" distinct from "declared
/// empty" — the two mean opposite things to `resolve_tools`.
fn optional_names<'py>(py: Python<'py>, names: Option<&[String]>) -> PyResult<Bound<'py, PyAny>> {
    match names {
        None => Ok(py.None().into_bound(py)),
        Some(names) => Ok(PyTuple::new(py, names)?.into_any()),
    }
}

fn agents_spec(spec: &AgentsSpec) -> PyAgentsSpec {
    let AgentsSpec {
        orchestrator:
            OrchestratorSpec {
                required,
                instruction,
            },
        participants: ParticipantSpec { min, max },
    } = spec;
    PyAgentsSpec {
        orchestrator: PyOrchestratorSpec {
            required: *required,
            instruction: instruction.clone(),
        },
        participants: PyParticipantSpec {
            min: *min,
            max: *max,
        },
    }
}

// ------------------------------------------------------------- GameplanConfig

/// One gameplan file: its contract, its prose, and where it came from.
#[pyclass(name = "GameplanConfig", module = "kerness._core", frozen)]
#[derive(Clone)]
pub struct PyGameplanConfig {
    pub inner: GameplanConfig,
}

#[pymethods]
impl PyGameplanConfig {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn harness(&self) -> PyHarnessSpec {
        PyHarnessSpec {
            inner: self.inner.harness.clone(),
        }
    }

    #[getter]
    fn body(&self) -> &str {
        &self.inner.body
    }

    #[getter]
    fn raw_text(&self) -> &str {
        &self.inner.raw_text
    }

    #[getter]
    fn path(&self) -> &str {
        &self.inner.path
    }

    /// The directory the file lives in, or `None` for a gameplan with no path.
    #[getter]
    fn directory(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match self.inner.directory() {
            None => Ok(py.None()),
            Some(path) => path_to_py(py, path.to_string_lossy().as_ref()),
        }
    }

    /// Whether the contract demands an orchestrator.
    #[getter]
    fn requires_orchestrator(&self) -> bool {
        self.inner.requires_orchestrator()
    }

    /// The round limit the contract declares.
    #[getter]
    fn max_rounds(&self) -> i64 {
        self.inner.max_rounds()
    }

    fn __repr__(&self) -> String {
        format!("GameplanConfig(name={})", repr_str(&self.inner.name))
    }
}

// ---------------------------------------------------------------- collections

/// Read a list of tool specs out of a Python sequence.
pub fn tool_specs(object: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Vec<ToolSpec>>> {
    let Some(object) = object.filter(|object| !object.is_none()) else {
        return Ok(None);
    };
    let mut specs = Vec::new();
    for item in object.try_iter()? {
        specs.push(item?.extract::<PyToolSpec>()?.inner);
    }
    Ok(Some(specs))
}

/// Render tool specs as the list a Python `chat` is handed.
pub fn tool_specs_to_py<'py>(py: Python<'py>, specs: &[ToolSpec]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for spec in specs {
        list.append(PyToolSpec::adopt(py, spec.clone()))?;
    }
    Ok(list)
}

/// Read a list of chat messages out of a Python sequence of dicts.
pub fn messages_from_py(object: &Bound<'_, PyAny>) -> PyResult<Vec<Value>> {
    let mut values = Vec::new();
    for item in object.try_iter()? {
        values.push(value_from_py(&item?)?);
    }
    Ok(values)
}

/// Render chat messages as a list of dicts.
pub fn messages_to_py<'py>(py: Python<'py>, values: &[Value]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for value in values {
        list.append(value_to_py(py, value)?)?;
    }
    Ok(list)
}

/// Read an optional `dict[str, str]` argument as an ordered header list.
pub fn optional_headers(object: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<(String, String)>> {
    let Some(object) = object.filter(|object| !object.is_none()) else {
        return Ok(Vec::new());
    };
    Ok(map_from_py(object.downcast::<PyDict>()?)?
        .into_iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(text) => text,
                other => other.to_string(),
            };
            (key, text)
        })
        .collect())
}
