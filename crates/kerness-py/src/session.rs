//! The session, and what a completed run reports.
//!
//! One difference from the Rust API is visible here: `Session::new` takes a
//! [`SessionConfig`] struct, while Python callers pass seventeen keyword
//! arguments. The struct is assembled in [`PySession::new`] and nowhere else,
//! so a Rust caller keeps the builder and a Python caller keeps the keywords.
//!
//! The `add_*` methods return the session itself, which is what makes the
//! upstream chaining style work. In Rust that is `&mut Self`; here it is the
//! same Python object handed back, so `Session(...).add_participant(...)`
//! builds one session rather than dropping it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kerness::agent::Role;
use kerness::session::{Session, SessionConfig, SessionResult};
use kerness::pyfmt::repr_str;
use kerness::tooling::{Arguments, ToolHandler};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::access::policy_from_py;
use crate::channel::bind_channel;
use crate::convert::{map_to_py, value_from_py};
use crate::errors::Raise;
use crate::provider::bind_provider;
use crate::types::{PyAgent, PyMemory, PyMessage};

/// What a completed run reports.
#[pyclass(name = "SessionResult", module = "kerness._core", frozen)]
pub struct PySessionResult {
    inner: SessionResult,
}

#[pymethods]
impl PySessionResult {
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

/// Orchestrates a multi-agent collaboration session.
#[pyclass(name = "Session", module = "kerness._core")]
pub struct PySession {
    inner: Session,
}

impl PySession {
    /// Build an agent from the keywords `add_participant` and
    /// `add_orchestrator` share, and give it *role*.
    #[allow(clippy::too_many_arguments)]
    fn agent(
        name: String,
        model: String,
        persona: String,
        role: Role,
        language: String,
        system_prompt: String,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        memory: Option<String>,
    ) -> PyResult<kerness::Agent> {
        Ok(kerness::Agent {
            name,
            model,
            persona,
            role,
            language,
            system_prompt,
            provider: match provider.filter(|object| !object.is_none()) {
                Some(object) => bind_provider(&object)?,
                None => None,
            },
            skills,
            memory,
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
        channel=None,
        memory="memory.md".to_string(),
        memory_write=false,
        session_file=None,
        max_context_tokens=256_000,
        access_policy=None,
        max_rounds=None,
        max_turns=None,
        max_tool_iterations=None,
        turn_delay_sec=1.0,
        show_reasoning=None,
        system_prompt="You are a participant in a structured debate. Be concise.".to_string(),
        orchestrator_retries=None,
        tool_results_in_history=false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        gameplan: String,
        topic: String,
        provider: Option<Bound<'_, PyAny>>,
        channel: Option<Bound<'_, PyAny>>,
        memory: String,
        memory_write: bool,
        session_file: Option<String>,
        max_context_tokens: usize,
        access_policy: Option<Bound<'_, PyAny>>,
        max_rounds: Option<i64>,
        max_turns: Option<i64>,
        max_tool_iterations: Option<u32>,
        turn_delay_sec: f64,
        show_reasoning: Option<bool>,
        system_prompt: String,
        orchestrator_retries: Option<i64>,
        tool_results_in_history: bool,
    ) -> PyResult<Self> {
        let config = SessionConfig {
            gameplan,
            topic,
            provider: match provider.filter(|object| !object.is_none()) {
                Some(object) => bind_provider(&object)?,
                None => None,
            },
            channel: match channel {
                Some(object) => bind_channel(&object)?,
                None => None,
            },
            memory,
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
        };
        Ok(PySession {
            inner: Session::new(config).raise()?,
        })
    }

    /// The agents registered so far.
    #[getter]
    fn _agents(&self) -> Vec<PyAgent> {
        self.inner
            .agents()
            .iter()
            .map(|agent| PyAgent {
                inner: agent.clone(),
                provider: None,
            })
            .collect()
    }

    /// The session-level memory file, live: reading it after `run()` reads
    /// what the run wrote.
    #[getter]
    fn memory(&self) -> PyMemory {
        PyMemory::of_session(self.inner.memories())
    }

    /// The rounds limit in force, after the gameplan's default is applied.
    #[getter]
    fn _max_rounds(&self) -> i64 {
        self.inner.max_rounds()
    }

    /// The allowed command regex patterns.
    #[getter]
    fn exec(&self) -> Vec<String> {
        self.inner.exec()
    }

    #[setter]
    fn set_exec(&mut self, patterns: Vec<String>) {
        self.inner.set_exec(patterns);
    }

    /// Add a participant agent to the session.
    #[pyo3(signature = (
        name,
        model=String::new(),
        persona=String::new(),
        language=String::new(),
        system_prompt=String::new(),
        provider=None,
        skills=None,
        memory=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn add_participant<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: String,
        model: String,
        persona: String,
        language: String,
        system_prompt: String,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        memory: Option<String>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let agent = PySession::agent(
            name,
            model,
            persona,
            Role::Participant,
            language,
            system_prompt,
            provider,
            skills,
            memory,
        )?;
        slf.inner.add_participant(agent);
        Ok(slf)
    }

    /// Add the session's one orchestrator.
    #[pyo3(signature = (
        name,
        model=String::new(),
        persona=String::new(),
        language=String::new(),
        system_prompt=String::new(),
        provider=None,
        skills=None,
        memory=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn add_orchestrator<'py>(
        mut slf: PyRefMut<'py, Self>,
        name: String,
        model: String,
        persona: String,
        language: String,
        system_prompt: String,
        provider: Option<Bound<'_, PyAny>>,
        skills: Option<Vec<String>>,
        memory: Option<String>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let agent = PySession::agent(
            name,
            model,
            persona,
            Role::Orchestrator,
            language,
            system_prompt,
            provider,
            skills,
            memory,
        )?;
        slf.inner.add_orchestrator(agent).raise()?;
        Ok(slf)
    }

    /// Attach a skill to the session.
    fn add_skill<'py>(mut slf: PyRefMut<'py, Self>, name: &str) -> PyResult<PyRefMut<'py, Self>> {
        slf.inner.add_skill(name).raise()?;
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
        slf.inner
            .add_tool(
                name,
                description,
                parameters,
                Arc::new(PyHandler { callable: handler }),
            )
            .raise()?;
        Ok(slf)
    }

    /// Run an external command with access control.
    #[pyo3(signature = (command, *, cwd=None, timeout_sec=60.0, actor=""))]
    fn run_command(
        &self,
        command: &str,
        cwd: Option<PathBuf>,
        timeout_sec: Option<f64>,
        actor: &str,
    ) -> PyResult<String> {
        self.inner
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
        self.inner.read_file(&path, actor).raise()
    }

    /// List a directory with access control.
    #[pyo3(signature = (path, *, actor=""))]
    fn list_dir(&self, path: &Bound<'_, PyAny>, actor: &str) -> PyResult<Vec<String>> {
        let path = path.str()?.to_string_lossy().into_owned();
        self.inner.list_dir(&path, actor).raise()
    }

    /// Execute the session, blocking until the loop terminates.
    fn run(&mut self) -> PyResult<PySessionResult> {
        Ok(PySessionResult {
            inner: self.inner.run().raise()?,
        })
    }
}
