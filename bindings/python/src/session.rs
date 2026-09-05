//! The session, and what a completed run reports.
//!
//! One difference from the Rust API is visible here: `Session::new` takes a
//! [`SessionConfig`] struct, while Python callers pass keyword
//! arguments. The struct is assembled in [`PySession::new`] and nowhere else,
//! so a Rust caller keeps the builder and a Python caller keeps the keywords.
//!
//! The `add_*` methods return the session itself, so that registration chains.
//! In Rust that is `&mut Self`; here it is the same Python object handed back,
//! so `Session(...).add_agent(...)` builds one session rather than
//! dropping it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kerness::context::ContextSource;
use kerness::exec::DEFAULT_TIMEOUT;
use kerness::memory::MemoryFilter;
use kerness::provider::ReasoningEffort;
use kerness::pyfmt::repr_str;
use kerness::role::Position;
use kerness::session::{
    ContextToolSpec, RunOptions, Session, SessionConfig, SessionResult, DEFAULT_MAX_CONTEXT_TOKENS,
};
use kerness::tooling::{Arguments, ToolHandler};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::access::policy_from_py;
use crate::channel::{bind_channel, PyChannel};
use crate::convert::{map_from_py, map_to_py, value_from_py};
use crate::errors::Raise;
use crate::memory::{bind_memory_store, PySessionMemory};
use crate::provider::bind_provider;
use crate::run::{require_callable, PyContextHandler, PyEventSink, PySessionRun};
use crate::types::{PyAgent, PyMessage, PyToolSpec};

/// What a completed run reports.
#[pyclass(name = "SessionResult", module = "kerness._core", frozen)]
pub struct PySessionResult {
    inner: SessionResult,
}

#[pymethods]
impl PySessionResult {
    #[new]
    #[pyo3(signature = (
        topic=String::new(),
        turns_completed=0,
        consensus_reached=false,
        history=Vec::new(),
        final_summary=String::new(),
        fields=None,
        rounds_run=0,
        phase_reached=String::new(),
        end_reason=String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        topic: String,
        turns_completed: i64,
        consensus_reached: bool,
        history: Vec<PyMessage>,
        final_summary: String,
        fields: Option<&Bound<'_, PyDict>>,
        rounds_run: i64,
        phase_reached: String,
        end_reason: String,
    ) -> PyResult<Self> {
        Ok(PySessionResult {
            inner: SessionResult {
                topic,
                turns_completed,
                consensus_reached,
                history: history.into_iter().map(|message| message.inner).collect(),
                final_summary,
                fields: match fields {
                    Some(dict) => map_from_py(dict)?,
                    None => Default::default(),
                },
                rounds_run,
                phase_reached,
                end_reason,
            },
        })
    }

    #[getter]
    fn topic(&self) -> &str {
        &self.inner.topic
    }

    #[getter]
    fn turns_completed(&self) -> i64 {
        self.inner.turns_completed
    }

    #[getter]
    fn consensus_reached(&self) -> bool {
        self.inner.consensus_reached
    }

    #[getter]
    fn history(&self) -> Vec<PyMessage> {
        self.inner
            .history
            .iter()
            .map(|message| PyMessage {
                inner: message.clone(),
            })
            .collect()
    }

    #[getter]
    fn final_summary(&self) -> &str {
        &self.inner.final_summary
    }

    /// The gameplan's declared `result:` fields, read out of the closing turn.
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
    fn end_reason(&self) -> &str {
        &self.inner.end_reason
    }

    /// The final summary, under its shorter name.
    #[getter]
    fn summary(&self) -> &str {
        self.inner.summary()
    }

    fn __repr__(&self) -> String {
        format!(
            "SessionResult(topic={}, turns_completed={}, end_reason={})",
            repr_str(&self.inner.topic),
            self.inner.turns_completed,
            repr_str(&self.inner.end_reason),
        )
    }
}

/// A Python callable registered as a tool handler.
struct PyHandler {
    callable: Py<PyAny>,
}

impl ToolHandler for PyHandler {
    fn call(&self, arguments: &Arguments, _actor: &str) -> kerness::Result<String> {
        use crate::errors::Catch;
        Python::with_gil(|py| {
            let arguments = map_to_py(py, arguments)?;
            let value = self.callable.bind(py).call1((arguments,))?;
            Ok(value.str()?.to_string_lossy().into_owned())
        })
        .catch()
    }
}

/// A Python callable behind the `MemoryFilter` trait.
///
/// Called with the note and the agent that wrote it, and expected to return the
/// text to store or `None` to drop the note.
struct PyFilter {
    callable: Py<PyAny>,
}

impl MemoryFilter for PyFilter {
    /// A filter that raises drops the note and says so.
    ///
    /// The trait has no error path, and it is a gate: the safe reading of a
    /// filter that could not decide is that the note does not go in the file.
    /// Silence would make that indistinguishable from a filter that returned
    /// `None` on purpose, so the failure is logged.
    fn filter(&self, note: &str, actor: &str) -> Option<String> {
        Python::with_gil(|py| match self.callable.bind(py).call1((note, actor)) {
            Ok(value) if value.is_none() => None,
            Ok(value) => Some(value.str().ok()?.to_string_lossy().into_owned()),
            Err(error) => {
                kerness::logging::warning(&format!(
                    "memory_filter raised on a note from {actor}; the note was \
                     dropped: {error}"
                ));
                None
            }
        })
    }
}

/// Wrap a caller's filter, treating `None` as "store notes as written".
///
/// A filter that is not callable is refused here rather than at the first note
/// an agent writes, mid-run and with the session's work already spent.
fn bind_memory_filter(filter: Option<Py<PyAny>>) -> PyResult<Option<Arc<dyn MemoryFilter>>> {
    let Some(callable) = filter else {
        return Ok(None);
    };
    let verdict = Python::with_gil(|py| -> PyResult<bool> {
        let bound = callable.bind(py);
        if bound.is_none() {
            return Ok(false);
        }
        if bound.is_callable() {
            return Ok(true);
        }
        Err(PyTypeError::new_err(format!(
            "memory_filter must be callable as filter(note, actor) and return \
             the text to store or None; got {}.",
            bound.get_type().name()?
        )))
    })?;
    Ok(verdict.then(|| Arc::new(PyFilter { callable }) as Arc<dyn MemoryFilter>))
}

/// A Python callable behind the `ContextSource` trait.
///
/// Called with the agent's name and expected to return text, so a caller writes
/// `lambda agent: ...` where the trait asks for an implementation — the same
/// bargain `add_tool` makes with a handler.
struct PySource {
    callable: Py<PyAny>,
}

impl ContextSource for PySource {
    fn render(&self, agent: &str) -> kerness::Result<String> {
        use crate::errors::Catch;
        Python::with_gil(|py| {
            let value = self.callable.bind(py).call1((agent,))?;
            Ok(value.str()?.to_string_lossy().into_owned())
        })
        .catch()
    }
}

/// Orchestrates a multi-agent collaboration session.
#[pyclass(name = "Session", module = "kerness._core")]
pub struct PySession {
    inner: Option<Session>,
    /// The channel the run writes to, when the caller wrote it in Python, kept
    /// so that an exception it raised can be re-raised from [`PySession::run`]
    /// rather than reported as the framework error it had to be reduced to on
    /// the way through. `None` for a bundled channel, which is Rust and reports
    /// through `Result` like the rest of the crate.
    channel: Option<Arc<PyChannel>>,
}

impl PySession {
    fn prepared(&self) -> PyResult<&Session> {
        self.inner
            .as_ref()
            .ok_or_else(|| {
                kerness::Error::session("Session already started; use its SessionRun handle.")
            })
            .raise()
    }

    fn prepared_mut(&mut self) -> PyResult<&mut Session> {
        self.inner
            .as_mut()
            .ok_or_else(|| {
                kerness::Error::session("Session already started; use its SessionRun handle.")
            })
            .raise()
    }

    /// Build an agent from `add_agent`'s keywords.
    #[allow(clippy::too_many_arguments)]
    fn agent(
        name: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        persona: Option<String>,
        role: Option<String>,
        language: Option<String>,
        system_prompt: Option<String>,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        tools: Option<Vec<String>>,
        memory: Option<String>,
        workspace: Option<String>,
    ) -> PyResult<kerness::Agent> {
        Ok(kerness::Agent {
            name,
            model,
            reasoning_effort: match reasoning_effort {
                Some(level) => Some(ReasoningEffort::parse(&level).raise()?),
                None => None,
            },
            persona,
            role,
            position: Position::Participant,
            language,
            system_prompt,
            provider: match provider.filter(|object| !object.is_none()) {
                Some(object) => bind_provider(&object)?,
                None => None,
            },
            skills,
            tools,
            memory,
            workspace,
        })
    }
}

#[pymethods]
impl PySession {
    #[new]
    #[pyo3(signature = (
        gameplan="debate".to_string(),
        topic=String::new(),
        provider=None,
        model=None,
        reasoning_effort="high".to_string(),
        persona=None,
        language=None,
        channel=None,
        memory="memory.md".to_string(),
        memory_write=false,
        memory_store=None,
        session_file=None,
        max_context_tokens=DEFAULT_MAX_CONTEXT_TOKENS,
        access_policy=None,
        max_rounds=None,
        max_turns=None,
        max_tool_iterations=None,
        turn_delay_sec=1.0,
        show_reasoning=None,
        system_prompt=None,
        orchestrator_retries=None,
        tool_results_in_history=false,
        memory_filter=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        gameplan: String,
        topic: String,
        provider: Option<Bound<'_, PyAny>>,
        model: Option<String>,
        reasoning_effort: String,
        persona: Option<String>,
        language: Option<String>,
        channel: Option<Bound<'_, PyAny>>,
        memory: String,
        memory_write: bool,
        memory_store: Option<Bound<'_, PyAny>>,
        session_file: Option<String>,
        max_context_tokens: usize,
        access_policy: Option<Bound<'_, PyAny>>,
        max_rounds: Option<i64>,
        max_turns: Option<i64>,
        max_tool_iterations: Option<u32>,
        turn_delay_sec: f64,
        show_reasoning: Option<bool>,
        system_prompt: Option<String>,
        orchestrator_retries: Option<i64>,
        tool_results_in_history: bool,
        memory_filter: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let bound_channel = match channel {
            Some(object) => bind_channel(&object)?,
            None => None,
        };
        let config = SessionConfig {
            gameplan,
            topic,
            provider: match provider.filter(|object| !object.is_none()) {
                Some(object) => bind_provider(&object)?,
                None => None,
            },
            model,
            reasoning_effort: ReasoningEffort::parse(&reasoning_effort).raise()?,
            persona,
            language,
            channel: bound_channel.as_ref().map(|bound| bound.channel.clone()),
            memory,
            memory_store: match memory_store {
                Some(object) => bind_memory_store(&object)?,
                None => None,
            },
            memory_write,
            session_file,
            max_context_tokens,
            access_policy: match access_policy.filter(|object| !object.is_none()) {
                Some(object) => Some(policy_from_py(&object)?),
                None => None,
            },
            max_rounds,
            max_turns,
            max_tool_iterations,
            turn_delay: Duration::from_secs_f64(turn_delay_sec.max(0.0)),
            show_reasoning,
            system_prompt,
            orchestrator_retries,
            tool_results_in_history,
            memory_filter: bind_memory_filter(memory_filter)?,
        };
        Ok(PySession {
            inner: Some(Session::new(config).raise()?),
            channel: bound_channel.and_then(|bound| bound.python),
        })
    }

    /// The agents registered so far.
    #[getter]
    fn _agents(&self) -> PyResult<Vec<PyAgent>> {
        Ok(self
            .prepared()?
            .agents()
            .iter()
            .map(|agent| PyAgent {
                inner: agent.clone(),
                provider: None,
            })
            .collect())
    }

    /// The session-level memory, live: reading it after `run()` reads what
    /// the run wrote.
    #[getter]
    fn memory(&self) -> PyResult<PySessionMemory> {
        Ok(PySessionMemory::of_session(self.prepared()?.memories()))
    }

    /// The rounds limit in force, after the gameplan's default is applied.
    #[getter]
    fn _max_rounds(&self) -> PyResult<i64> {
        Ok(self.prepared()?.max_rounds())
    }

    /// The allowed command regex patterns.
    #[getter]
    fn exec(&self) -> PyResult<Vec<String>> {
        Ok(self.prepared()?.exec())
    }

    #[setter]
    fn set_exec(&mut self, patterns: Vec<String>) -> PyResult<()> {
        self.prepared_mut()?.set_exec(patterns);
        Ok(())
    }

    /// Add an agent to the session.
    ///
    /// Every keyword defaults to `None`, meaning *unset*: the session's value
    /// fills it at `run()`. An empty string is a value, not an absence.
    ///
    /// `role` is the exception — it has no session-level default, because a
    /// session-wide role would make every agent the orchestrator at once.
    /// Unset seats a participant. A built-in name or a path to a `.md` role
    /// file is read here and now, so a typo raises at this call; anything else
    /// is that agent's job written out as prose, and prose always seats a
    /// participant.
    #[pyo3(signature = (
        name,
        model=None,
        reasoning_effort=None,
        persona=None,
        role=None,
        language=None,
        system_prompt=None,
        provider=None,
        skills=None,
        tools=None,
        memory=None,
        workspace=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn add_agent<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        persona: Option<String>,
        role: Option<String>,
        language: Option<String>,
        system_prompt: Option<String>,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        tools: Option<Vec<String>>,
        memory: Option<String>,
        workspace: Option<String>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let agent = PySession::agent(
            name,
            model,
            reasoning_effort,
            persona,
            role,
            language,
            system_prompt,
            provider,
            skills,
            tools,
            memory,
            workspace,
        )?;
        slf.prepared_mut()?.add_agent(agent).raise()?;
        Ok(slf)
    }

    /// Attach a skill to the session.
    fn add_skill<'py>(mut slf: PyRefMut<'py, Self>, name: &str) -> PyResult<PyRefMut<'py, Self>> {
        slf.prepared_mut()?.add_skill(name).raise()?;
        Ok(slf)
    }

    /// Register a callable tool for agents to invoke.
    fn add_tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: &str,
        description: &str,
        parameters: &Bound<'_, PyAny>,
        handler: Py<PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let parameters = value_from_py(parameters)?;
        slf.prepared_mut()?
            .add_tool(
                name,
                description,
                parameters,
                Arc::new(PyHandler { callable: handler }),
            )
            .raise()?;
        Ok(slf)
    }

    /// Register a complete ToolSpec, preserving its actor-aware handler.
    fn add_tool_spec<'py>(
        mut slf: PyRefMut<'py, Self>,
        spec: &PyToolSpec,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.prepared_mut()?
            .add_tool_spec(spec.inner.clone())
            .raise()?;
        Ok(slf)
    }

    /// Register handler(arguments, context) and optional preflight(arguments, identity).
    #[pyo3(signature = (name, description, parameters, handler, *, preflight=None))]
    fn add_contextual_tool<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: String,
        description: String,
        parameters: &Bound<'_, PyAny>,
        handler: Py<PyAny>,
        preflight: Option<Py<PyAny>>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        require_callable(handler.bind(slf.py()), "handler")?;
        if let Some(callable) = &preflight {
            require_callable(callable.bind(slf.py()), "preflight")?;
        }
        let spec = ContextToolSpec {
            name,
            description,
            parameters: value_from_py(parameters)?,
            handler: Arc::new(PyContextHandler { handler, preflight }),
        };
        slf.prepared_mut()?.add_contextual_tool(spec).raise()?;
        Ok(slf)
    }

    /// Register standing background text for agents to read.
    ///
    /// *source* is called with one argument, the agent's name, once per agent
    /// at the top of :meth:`run`.
    fn add_context<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: &str,
        source: Py<PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.prepared_mut()?
            .add_context(name, Arc::new(PySource { callable: source }))
            .raise()?;
        Ok(slf)
    }

    /// Run an external command with access control.
    #[pyo3(signature = (command, *, cwd=None, timeout_sec=DEFAULT_TIMEOUT.as_secs_f64(), actor=""))]
    fn run_command(
        &self,
        command: &str,
        cwd: Option<PathBuf>,
        timeout_sec: Option<f64>,
        actor: &str,
    ) -> PyResult<String> {
        self.prepared()?
            .run_command(
                command,
                cwd.as_deref(),
                timeout_sec.map(|secs| Duration::from_secs_f64(secs.max(0.0))),
                actor,
            )
            .raise()
    }

    /// Read a file with access control.
    #[pyo3(signature = (path, *, actor=""))]
    fn read_file(&self, path: &Bound<'_, PyAny>, actor: &str) -> PyResult<String> {
        let path = path.str()?.to_string_lossy().into_owned();
        self.prepared()?.read_file(&path, actor).raise()
    }

    /// List a directory with access control.
    #[pyo3(signature = (path, *, actor=""))]
    fn list_dir(&self, path: &Bound<'_, PyAny>, actor: &str) -> PyResult<Vec<String>> {
        let path = path.str()?.to_string_lossy().into_owned();
        self.prepared()?.list_dir(&path, actor).raise()
    }

    /// Transfer this configuration into an owned run controlled through step().
    /// This Session is consumed even when Rust preparation fails.
    #[pyo3(signature = (
        *, mode="automatic", approvals="external", budget=None, pricing=None,
        event_sink=None, result_validation="strict", binding_version=String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        mode: &str,
        approvals: &str,
        budget: Option<&Bound<'_, PyAny>>,
        pricing: Option<&Bound<'_, PyAny>>,
        event_sink: Option<Py<PyAny>>,
        result_validation: &str,
        binding_version: String,
    ) -> PyResult<PySessionRun> {
        let mut options = RunOptions {
            mode: serde_json::from_value(mode.into())
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            approvals: serde_json::from_value(approvals.into())
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            result_validation: serde_json::from_value(result_validation.into())
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            binding_version,
            ..RunOptions::default()
        };
        if let Some(budget) = budget.filter(|object| !object.is_none()) {
            options.budget = serde_json::from_value(value_from_py(budget)?)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
        }
        if let Some(pricing) = pricing.filter(|object| !object.is_none()) {
            options.pricing = serde_json::from_value(value_from_py(pricing)?)
                .map_err(|error| PyValueError::new_err(error.to_string()))?;
        }
        if let Some(callable) = event_sink {
            Python::with_gil(|py| require_callable(callable.bind(py), "event_sink"))?;
            options.event_sink = Some(Arc::new(PyEventSink { callable }));
        }
        self.prepared()?;
        let session = self.inner.take().expect("prepared above");
        let started = session.start(options);
        if let Some(raised) = self.channel.as_ref().and_then(|channel| channel.parked()) {
            return Err(raised);
        }
        Ok(PySessionRun {
            inner: started.raise()?,
            channel: self.channel.clone(),
        })
    }

    /// Execute the session, blocking until the loop terminates.
    fn run(&mut self) -> PyResult<PySessionResult> {
        let finished = self.prepared_mut()?.run();
        if let Some(raised) = self.channel.as_ref().and_then(|channel| channel.parked()) {
            return Err(raised);
        }
        Ok(PySessionResult {
            inner: finished.raise()?,
        })
    }
}
