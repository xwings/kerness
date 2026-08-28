//! The pieces a caller drives a session with, one turn at a time.
//!
//! `Conversation`, `ToolDispatcher`, `PromptAssembler`, `AgentRunner` and
//! `OrchestratorLoop` are what `Session` is assembled from, and each is usable
//! on its own, which is what lets a harness that wants a different loop keep
//! everything else.
//!
//! Two of them borrow in Rust and cannot in Python. [`PromptAssembler`] and
//! [`AgentRunner`] hold references to the state they read through, which no
//! `#[pyclass]` can outlive a method call. So the Python classes own the
//! pieces — the agent, the provider, the callables — and build the borrowing
//! Rust value inside each call. The cost is one construction per turn; the
//! alternative is a lifetime a Python object cannot express.

use std::cell::RefCell;
use std::sync::Arc;

use kerness::agent::Agent;
use kerness::agent_runtime::AgentRunner;
use kerness::conversation::{ChatMessage, Conversation};
use kerness::orchestrator::{LoopHost, LoopState, OrchestratorLoop};
use kerness::prompting::PromptAssembler;
use kerness::provider::Provider;
use kerness::pyfmt::repr_str;
use kerness::tooling::ToolSpec;
use kerness::toolkit::{ToolDispatcher, ToolsFor};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::{Map, Value};

use crate::convert::{map_from_py, map_to_py, value_to_py};
use crate::errors::{Catch, Raise};
use crate::provider::bind_provider;
use crate::types::{
    messages_from_py, messages_to_py, PyAgent, PyLoopSpec, PyMessage, PyResultField, PyToolCall,
    PyToolResult, PyToolSpec, PyTurn,
};

/// A parked Python exception, re-raised once the Rust call has unwound.
///
/// The framework's callback types return plain values, not results — a
/// `Fn(&Agent) -> String` has nowhere to put a failure. Rather than let a
/// raising callable read as an empty answer, the exception is parked here and
/// raised at the boundary, which is where the caller expects it.
type Parked = RefCell<Option<PyErr>>;

fn park<T: Default>(parked: &Parked, result: PyResult<T>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            parked.borrow_mut().get_or_insert(error);
            T::default()
        }
    }
}

fn unpark(parked: Parked) -> PyResult<()> {
    match parked.into_inner() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn specs_from(object: &Bound<'_, PyAny>) -> PyResult<Vec<ToolSpec>> {
    Ok(object
        .extract::<Vec<PyToolSpec>>()?
        .into_iter()
        .map(|spec| spec.inner)
        .collect())
}

// -------------------------------------------------------------- Conversation

/// The running history, in both the shapes a session needs it.
#[pyclass(name = "Conversation", module = "kerness._core")]
#[derive(Default)]
pub struct PyConversation {
    pub inner: Conversation,
}

#[pymethods]
impl PyConversation {
    #[new]
    fn new() -> Self {
        PyConversation::default()
    }

    /// Append a user-role directive.
    fn directive(&mut self, content: &str) {
        self.inner.directive(content);
    }

    /// Append a message at *role*, verbatim.
    fn raw(&mut self, role: &str, content: &str) {
        self.inner.raw(role, content);
    }

    /// Record one agent's turn, for both the model and the transcript.
    #[pyo3(signature = (speaker, content, round_idx=0, msg_type="turn"))]
    fn say(&mut self, speaker: &str, content: &str, round_idx: i64, msg_type: &str) {
        self.inner.say(speaker, content, round_idx, msg_type);
    }

    /// Record a system note.
    fn note(&mut self, content: &str) {
        self.inner.note(content);
    }

    /// The chat messages the model is shown.
    fn render<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        chat_messages(py, &self.inner.render())
    }

    /// The turns, in order.
    fn turns(&self) -> Vec<PyTurn> {
        self.inner
            .turns()
            .iter()
            .map(|turn| PyTurn {
                inner: turn.clone(),
            })
            .collect()
    }

    /// The public transcript, in order.
    fn transcript(&self) -> Vec<PyMessage> {
        self.inner
            .transcript()
            .iter()
            .map(|message| PyMessage {
                inner: message.clone(),
            })
            .collect()
    }

    /// Swap in a compacted turn list, leaving the transcript alone.
    fn replace_turns(&mut self, turns: Vec<PyTurn>) {
        self.inner
            .replace_turns(turns.into_iter().map(|turn| turn.inner).collect());
    }

    /// Put a saved conversation back.
    fn restore(&mut self, turns: Vec<PyTurn>, transcript: Vec<PyMessage>) {
        self.inner.restore(
            turns.into_iter().map(|turn| turn.inner).collect(),
            transcript
                .into_iter()
                .map(|message| message.inner)
                .collect(),
        );
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }
}

fn chat_messages<'py>(py: Python<'py>, chat: &[ChatMessage]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for message in chat {
        let dict = PyDict::new(py);
        dict.set_item("role", &message.role)?;
        dict.set_item("content", &message.content)?;
        list.append(dict)?;
    }
    Ok(list)
}

// ------------------------------------------------------------- ToolDispatcher

/// Runs tool calls against whatever is permitted at the moment of the call.
#[pyclass(name = "ToolDispatcher", module = "kerness._core")]
pub struct PyToolDispatcher {
    pub inner: ToolDispatcher,
}

#[pymethods]
impl PyToolDispatcher {
    #[new]
    fn new(tools_for: &Bound<'_, PyAny>) -> Self {
        let lookup = tools_for.clone().unbind();
        // Dispatch never fails, so neither does this: a lookup that raises
        // yields no tools, and the call comes back as "Unknown tool" — an
        // error the model is shown and can act on.
        let tools_for: ToolsFor = Arc::new(move || {
            Python::with_gil(|py| {
                lookup
                    .bind(py)
                    .call0()
                    .and_then(|found| specs_from(&found))
                    .unwrap_or_default()
            })
        });
        PyToolDispatcher {
            inner: ToolDispatcher::new(tools_for),
        }
    }

    /// Run one tool call and describe what happened.
    #[pyo3(signature = (call, actor=""))]
    fn execute(&self, call: PyRef<'_, PyToolCall>, actor: &str) -> PyToolResult {
        PyToolResult::adopt(self.inner.execute(&call.inner, actor))
    }
}

// ------------------------------------------------------------ PromptAssembler

/// Builds system prompts and message lists from session state it reads back
/// through the caller's callables.
#[pyclass(name = "PromptAssembler", module = "kerness._core")]
pub struct PyPromptAssembler {
    skills_for: Py<PyAny>,
    memory_for: Py<PyAny>,
    tools_for: Py<PyAny>,
    dialect_for: Option<Py<PyAny>>,
    show_reasoning: Option<bool>,
    memory_writable: bool,
}

impl PyPromptAssembler {
    /// Build the borrowing assembler for one call.
    ///
    /// *agent* is handed straight back to the callables rather than rebuilt
    /// from the framework struct, so a caller keying a dict on the object it
    /// passed in still finds its entry.
    fn assembler<'a>(
        &'a self,
        agent: &'a Bound<'a, PyAny>,
        parked: &'a Parked,
    ) -> PromptAssembler<'a> {
        let py = agent.py();
        let skills_for = move |_: &Agent| -> String {
            park(
                parked,
                self.skills_for
                    .bind(py)
                    .call1((agent,))
                    .and_then(|found| found.extract()),
            )
        };
        // `memory_for` hands back the agent's memory, not its text, so that a
        // caller can point two agents at the same file and have both see a
        // write. Reading it here is what keeps the core taking plain content.
        let memory_for = move |_: &Agent| -> String {
            park(
                parked,
                self.memory_for
                    .bind(py)
                    .call1((agent,))
                    .and_then(|memory| memory.call_method0("read"))
                    .and_then(|content| content.extract()),
            )
        };
        let tools_for = move || -> Vec<ToolSpec> {
            park(
                parked,
                self.tools_for
                    .bind(py)
                    .call0()
                    .and_then(|found| specs_from(&found)),
            )
        };
        let assembler =
            PromptAssembler::new(skills_for, memory_for, tools_for, self.show_reasoning)
                .with_memory_writable(self.memory_writable);
        match &self.dialect_for {
            None => assembler,
            Some(callable) => assembler.with_dialect(move |_: &Agent| {
                // A dialect that cannot be resolved is text: it is the dialect
                // every provider speaks, so the prompt still carries the tools.
                park(
                    parked,
                    callable
                        .bind(py)
                        .call1((agent,))
                        .and_then(|found| crate::types::dialect_from_py(&found)),
                )
            }),
        }
    }

    /// Run *build* against a freshly-bound assembler, re-raising any Python
    /// exception a callable parked on the way through.
    fn with<T>(
        &self,
        agent: &Bound<'_, PyAny>,
        build: impl FnOnce(&PromptAssembler<'_>, &Agent) -> kerness::Result<T>,
    ) -> PyResult<T> {
        let core = agent.extract::<PyRef<'_, PyAgent>>()?.snapshot();
        let parked = Parked::default();
        let built = build(&self.assembler(agent, &parked), &core);
        unpark(parked)?;
        built.raise()
    }
}

#[pymethods]
impl PyPromptAssembler {
    #[new]
    #[pyo3(signature = (
        *,
        skills_for,
        memory_for,
        tools_for,
        show_reasoning=None,
        dialect_for=None,
        memory_writable=false,
    ))]
    fn new(
        skills_for: Py<PyAny>,
        memory_for: Py<PyAny>,
        tools_for: Py<PyAny>,
        show_reasoning: Option<bool>,
        dialect_for: Option<Bound<'_, PyAny>>,
        memory_writable: bool,
    ) -> Self {
        PyPromptAssembler {
            skills_for,
            memory_for,
            tools_for,
            dialect_for: dialect_for
                .filter(|object| !object.is_none())
                .map(Bound::unbind),
            show_reasoning,
            memory_writable,
        }
    }

    /// The orchestrator's system prompt, built from the gameplan prompt.
    fn orchestrator_system(&self, agent: &Bound<'_, PyAny>, base_prompt: &str) -> PyResult<String> {
        self.with(agent, |assembler, core| {
            assembler.orchestrator_system(core, base_prompt)
        })
    }

    /// A participant's full message list, system message first.
    fn participant_messages<'py>(
        &self,
        py: Python<'py>,
        agent: &Bound<'_, PyAny>,
        history: &Bound<'_, PyAny>,
        base_prompt: &str,
    ) -> PyResult<Bound<'py, PyList>> {
        let history = messages_from_py(history)?;
        let built = self.with(agent, |assembler, core| {
            assembler.participant_messages(core, &history, base_prompt)
        })?;
        messages_to_py(py, &built)
    }

    /// The message list for any agent, by role.
    fn messages_for<'py>(
        &self,
        py: Python<'py>,
        agent: &Bound<'_, PyAny>,
        history: &Bound<'_, PyAny>,
        base_prompt: &str,
    ) -> PyResult<Bound<'py, PyList>> {
        let history = messages_from_py(history)?;
        let built = self.with(agent, |assembler, core| {
            assembler.messages_for(core, &history, base_prompt)
        })?;
        messages_to_py(py, &built)
    }
}

// ---------------------------------------------------------------- AgentRunner

/// Runs a single agent turn, including its private tool loop.
#[pyclass(name = "AgentRunner", module = "kerness._core")]
pub struct PyAgentRunner {
    agent: Py<PyAny>,
    core: Agent,
    provider: Arc<dyn Provider>,
    messages_for: Py<PyAny>,
    dispatcher: Py<PyToolDispatcher>,
    base_prompt: String,
    max_tool_iterations: Option<u32>,
    record: Option<Py<PyAny>>,
    tools_for: Option<Py<PyAny>>,
}

#[pymethods]
impl PyAgentRunner {
    #[new]
    #[pyo3(signature = (
        *,
        agent,
        provider,
        messages_for,
        dispatcher,
        base_prompt,
        max_tool_iterations=None,
        record_tool_exchange=None,
        tools_for=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        agent: Bound<'_, PyAny>,
        provider: Bound<'_, PyAny>,
        messages_for: Py<PyAny>,
        dispatcher: Py<PyToolDispatcher>,
        base_prompt: String,
        max_tool_iterations: Option<u32>,
        record_tool_exchange: Option<Bound<'_, PyAny>>,
        tools_for: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let core = agent.extract::<PyRef<'_, PyAgent>>()?.snapshot();
        let provider = bind_provider(&provider)?.ok_or_else(|| {
            crate::errors::to_py(kerness::Error::Value(
                "AgentRunner needs a provider".to_string(),
            ))
        })?;
        Ok(PyAgentRunner {
            agent: agent.unbind(),
            core,
            provider,
            messages_for,
            dispatcher,
            base_prompt,
            max_tool_iterations,
            record: record_tool_exchange
                .filter(|object| !object.is_none())
                .map(Bound::unbind),
            tools_for: tools_for
                .filter(|object| !object.is_none())
                .map(Bound::unbind),
        })
    }

    /// Take one turn and return the agent's final text.
    #[pyo3(signature = (history, purpose, instruction=None))]
    fn run(
        &self,
        py: Python<'_>,
        history: &Bound<'_, PyAny>,
        purpose: &str,
        instruction: Option<&str>,
    ) -> PyResult<String> {
        let history = messages_from_py(history)?;
        let dispatcher = self.dispatcher.bind(py).borrow();
        let parked = Parked::default();

        let agent = self.agent.bind(py);
        let messages_for = |_: &Agent, history: &[Value], base_prompt: &str| {
            let history = messages_to_py(py, history)?;
            let built = self
                .messages_for
                .bind(py)
                .call1((agent, history, base_prompt))?;
            messages_from_py(&built)
        };

        let mut runner = AgentRunner::new(
            &self.core,
            self.provider.as_ref(),
            |agent: &Agent, history: &[Value], base_prompt: &str| {
                messages_for(agent, history, base_prompt).catch()
            },
            &dispatcher.inner,
            self.base_prompt.clone(),
        );
        if let Some(limit) = self.max_tool_iterations {
            runner = runner.with_max_tool_iterations(limit);
        }
        if let Some(record) = &self.record {
            runner = runner.with_record(|message: &Value| {
                // The record is a sink for the shared history; a caller whose
                // sink raises still gets the turn, and the exception surfaces
                // at the end of it.
                park(
                    &parked,
                    value_to_py(py, message).and_then(|message| {
                        record.bind(py).call1((message,))?;
                        Ok(())
                    }),
                );
            });
        }
        if let Some(tools_for) = &self.tools_for {
            runner = runner.with_tools(|| {
                park(
                    &parked,
                    tools_for
                        .bind(py)
                        .call0()
                        .and_then(|found| specs_from(&found)),
                )
            });
        }

        let text = runner.run(&history, purpose, instruction);
        drop(runner);
        unpark(parked)?;
        text.raise()
    }
}

// ------------------------------------------------------------------ LoopState

/// Everything the loop tracks across turns.
#[pyclass(name = "LoopState", module = "kerness._core")]
#[derive(Clone, Default)]
pub struct PyLoopState {
    pub inner: LoopState,
}

#[pymethods]
impl PyLoopState {
    #[getter]
    fn turn_count(&self) -> i64 {
        self.inner.turn_count
    }

    #[getter]
    fn consensus_reached(&self) -> bool {
        self.inner.consensus_reached
    }

    #[getter]
    fn final_summary(&self) -> &str {
        &self.inner.final_summary
    }

    #[getter]
    fn fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.fields)
    }

    #[getter]
    fn rounds_run(&self) -> i64 {
        self.inner.rounds_run
    }

    #[getter]
    fn phase_reached(&self) -> &str {
        &self.inner.phase_reached
    }

    #[getter]
    fn end_reason(&self) -> &'static str {
        self.inner.end_reason.as_str()
    }

    fn __repr__(&self) -> String {
        format!(
            "LoopState(turn_count={}, rounds_run={}, end_reason={})",
            self.inner.turn_count,
            self.inner.rounds_run,
            repr_str(self.inner.end_reason.as_str()),
        )
    }
}

// ----------------------------------------------------------- OrchestratorLoop

/// A Python host, seen as a [`LoopHost`].
struct PyHost<'py> {
    py: Python<'py>,
    host: &'py Bound<'py, PyAny>,
}

impl LoopHost for PyHost<'_> {
    fn orchestrator_turn(
        &mut self,
        purpose: &str,
        instruction: Option<&str>,
    ) -> kerness::Result<String> {
        self.host
            .call_method1("orchestrator_turn", (purpose, instruction))
            .and_then(|reply| reply.extract())
            .catch()
    }

    fn participant_turn(&mut self, name: &str, instruction: &str) -> kerness::Result<String> {
        self.host
            .call_method1("participant_turn", (name, instruction))
            .and_then(|reply| reply.extract())
            .catch()
    }

    fn deliver(
        &mut self,
        sender: &str,
        text: &str,
        turn: i64,
        msg_type: &str,
    ) -> kerness::Result<()> {
        self.host
            .call_method1("deliver", (sender, text, turn, msg_type))
            .map(drop)
            .catch()
    }

    fn note(&mut self, message: &str) -> kerness::Result<()> {
        self.host.call_method1("note", (message,)).map(drop).catch()
    }

    fn directive(&mut self, text: &str) -> kerness::Result<()> {
        self.host
            .call_method1("directive", (text,))
            .map(drop)
            .catch()
    }

    fn closing_turn(&mut self, prompt: &str) -> kerness::Result<String> {
        self.host
            .call_method1("closing_turn", (prompt,))
            .and_then(|reply| reply.extract())
            .catch()
    }

    fn record_summary(&mut self, text: &str, turn: i64) -> kerness::Result<()> {
        self.host
            .call_method1("record_summary", (text, turn))
            .map(drop)
            .catch()
    }

    fn record_position(&mut self, snapshot: Map<String, Value>) {
        // Optional on the Python side, and a failure here is not the run's
        // problem: the position is an aid to resuming, not part of the turn.
        let _ = map_to_py(self.py, &snapshot)
            .and_then(|snapshot| self.host.call_method1("record_position", (snapshot,)));
    }
}

/// Runs one session's worth of turns.
#[pyclass(name = "OrchestratorLoop", module = "kerness._core")]
pub struct PyOrchestratorLoop {
    inner: OrchestratorLoop,
    host: Py<PyAny>,
}

#[pymethods]
impl PyOrchestratorLoop {
    #[new]
    #[pyo3(signature = (
        *,
        spec,
        host,
        orchestrator_name,
        participant_names,
        result_fields=None,
        max_turns=None,
        retries=None,
        resume_state=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        spec: PyRef<'_, PyLoopSpec>,
        host: Py<PyAny>,
        orchestrator_name: String,
        participant_names: Vec<String>,
        result_fields: Option<Vec<PyResultField>>,
        max_turns: Option<i64>,
        retries: Option<i64>,
        resume_state: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let mut inner =
            OrchestratorLoop::new(spec.inner.clone(), orchestrator_name, participant_names);
        if let Some(fields) = result_fields {
            inner = inner.with_result_fields(fields.iter().map(PyResultField::snapshot).collect());
        }
        if let Some(max_turns) = max_turns {
            inner = inner.with_max_turns(max_turns);
        }
        if let Some(retries) = retries {
            inner = inner.with_retries(retries);
        }
        if let Some(state) = resume_state {
            inner = inner.with_resume_state(map_from_py(state)?);
        }
        Ok(PyOrchestratorLoop { inner, host })
    }

    /// Run the session to its end and return the final state.
    fn run(&mut self, py: Python<'_>) -> PyResult<PyLoopState> {
        let host = self.host.bind(py);
        let mut host = PyHost { py, host };
        Ok(PyLoopState {
            inner: self.inner.run(&mut host).raise()?,
        })
    }

    /// The loop's position, for a caller that persists it.
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.snapshot())
    }
}
