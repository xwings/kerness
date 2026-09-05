//! Owned runtime handles and contextual callbacks, translated at the boundary.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kerness::exec::DEFAULT_TIMEOUT;
use kerness::session::{
    ContextToolHandler, EventSink, PreflightAction, RunControl, RunEvent, RunInput, SessionRun,
    ToolContext, ToolIdentity,
};
use kerness::tooling::Arguments;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

use crate::channel::PyChannel;
use crate::convert::{map_to_py, value_from_py, value_to_py};
use crate::errors::{Catch, Raise};

pub(crate) fn serialized<'py>(
    py: Python<'py>,
    value: serde_json::Result<Value>,
) -> PyResult<Bound<'py, PyAny>> {
    value_to_py(
        py,
        &value.map_err(|error| PyValueError::new_err(error.to_string()))?,
    )
}

pub(crate) fn require_callable(object: &Bound<'_, PyAny>, name: &str) -> PyResult<()> {
    if object.is_callable() {
        Ok(())
    } else {
        Err(PyTypeError::new_err(format!("{name} must be callable.")))
    }
}

/// The callback receives a detached handle to one Rust invocation.
pub(crate) struct PyContextHandler {
    pub handler: Py<PyAny>,
    pub preflight: Option<Py<PyAny>>,
}

impl ContextToolHandler for PyContextHandler {
    fn preflight(
        &self,
        arguments: &Arguments,
        identity: &ToolIdentity,
    ) -> kerness::Result<Option<PreflightAction>> {
        let Some(callable) = &self.preflight else {
            return Ok(None);
        };
        Python::with_gil(|py| {
            let result = callable.bind(py).call1((
                map_to_py(py, arguments)?,
                serialized(py, serde_json::to_value(identity))?,
            ))?;
            if result.is_none() {
                Ok(None)
            } else {
                serde_json::from_value(value_from_py(&result)?)
                    .map(Some)
                    .map_err(|error| PyValueError::new_err(error.to_string()))
            }
        })
        .catch()
    }

    fn call(&self, arguments: &Arguments, context: &ToolContext) -> kerness::Result<String> {
        Python::with_gil(|py| {
            let context = Py::new(
                py,
                PyToolContext {
                    inner: context.clone(),
                },
            )?;
            let value = self
                .handler
                .bind(py)
                .call1((map_to_py(py, arguments)?, context))?;
            Ok(value.str()?.to_string_lossy().into_owned())
        })
        .catch()
    }
}

pub(crate) struct PyEventSink {
    pub callable: Py<PyAny>,
}

impl EventSink for PyEventSink {
    fn emit(&self, event: &RunEvent) -> kerness::Result<()> {
        Python::with_gil(|py| {
            self.callable
                .bind(py)
                .call1((serialized(py, serde_json::to_value(event))?,))
                .map(drop)
        })
        .catch()
    }
}

/// Trusted invocation identity and capabilities that expire after the callback.
#[pyclass(name = "ToolContext", module = "kerness._core", frozen)]
pub struct PyToolContext {
    inner: ToolContext,
}

#[pymethods]
impl PyToolContext {
    #[getter]
    fn actor(&self) -> &str {
        self.inner.identity().actor()
    }

    #[getter]
    fn run_id(&self) -> &str {
        self.inner.identity().run_id()
    }

    #[getter]
    fn turn_id(&self) -> u64 {
        self.inner.identity().turn_id()
    }

    #[getter]
    fn call_id(&self) -> &str {
        self.inner.identity().call_id()
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn read_file(&self, path: &Bound<'_, PyAny>) -> PyResult<String> {
        self.inner.read_file(&path.str()?.to_string_lossy()).raise()
    }

    fn list_dir(&self, path: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
        self.inner.list_dir(&path.str()?.to_string_lossy()).raise()
    }

    fn read_memory(&self) -> PyResult<String> {
        self.inner.read_memory().raise()
    }

    fn write_memory(&self, note: &str) -> PyResult<bool> {
        self.inner.write_memory(note).raise()
    }

    #[pyo3(signature = (command, *, cwd=None, timeout_sec=DEFAULT_TIMEOUT.as_secs_f64()))]
    fn run_command(
        &self,
        py: Python<'_>,
        command: &str,
        cwd: Option<PathBuf>,
        timeout_sec: Option<f64>,
    ) -> PyResult<String> {
        let timeout = timeout_sec
            .map(|seconds| {
                Duration::try_from_secs_f64(seconds.max(0.0))
                    .map_err(|error| PyValueError::new_err(error.to_string()))
            })
            .transpose()?;
        py.allow_threads(|| self.inner.run_command(command, cwd.as_deref(), timeout))
            .raise()
    }
}

/// Cancellation independent of a borrowed SessionRun.
#[pyclass(name = "RunControl", module = "kerness._core", frozen)]
pub struct PyRunControl {
    inner: RunControl,
}

#[pymethods]
impl PyRunControl {
    fn cancel(&self) {
        self.inner.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

/// An owned Rust run. Inputs and outcomes use the engine's JSON schemas.
#[pyclass(name = "SessionRun", module = "kerness._core")]
pub struct PySessionRun {
    pub(crate) inner: SessionRun,
    pub(crate) channel: Option<Arc<PyChannel>>,
}

#[pymethods]
impl PySessionRun {
    /// Advance one engine-selected operation or report a waiting/terminal state.
    #[pyo3(signature = (input=None))]
    fn step<'py>(
        &mut self,
        py: Python<'py>,
        input: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let input = match input.filter(|value| !value.is_none()) {
            Some(value) => serde_json::from_value(value_from_py(value)?)
                .map_err(|error| PyValueError::new_err(error.to_string()))?,
            None => RunInput::Continue,
        };
        let result = py.allow_threads(|| self.inner.step(input));
        if let Some(raised) = self.channel.as_ref().and_then(|channel| channel.parked()) {
            return Err(raised);
        }
        serialized(py, serde_json::to_value(result.raise()?))
    }

    fn control(&self) -> PyRunControl {
        PyRunControl {
            inner: self.inner.control(),
        }
    }

    fn checkpoint(&self) -> PyResult<()> {
        self.inner.checkpoint().raise()
    }

    fn drain_events<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialized(py, serde_json::to_value(self.inner.drain_events()))
    }

    fn outcome<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialized(py, serde_json::to_value(self.inner.outcome()))
    }

    fn usage<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        serialized(py, serde_json::to_value(self.inner.usage()))
    }
}
