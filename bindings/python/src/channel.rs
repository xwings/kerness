//! Output channels, and where the framework's own diagnostics go.
//!
//! The four bundled channels are written in Python, unlike everything else
//! here, and for the same reason in each case: what they do *is* Python I/O.
//! `ConsoleChannel` is a `print`, so it has to reach `sys.stdout` and not file
//! descriptor 1; `MultiChannel` reports a failed delivery through `logging`, so
//! it has to reach the caller's handlers. Rust would be writing past both.
//!
//! What Rust does own is the other direction: a channel the caller wrote, seen
//! by the framework as a [`Channel`], and the framework's diagnostics arriving
//! in Python's `logging` rather than on stderr.

use std::sync::{Arc, Mutex};

use kerness::channel::Channel;
use kerness::error::Result;
use kerness::logging::{Level, Logger};
use pyo3::prelude::*;

use crate::errors::Catch;

/// A Python channel, seen as a framework [`Channel`].
pub struct PyChannel {
    inner: Py<PyAny>,
    /// `type(inner).__name__`, read once: [`Channel::type_name`] is only ever
    /// read by a failure log, and it has to name something the caller can go
    /// and fix.
    name: String,
    /// A channel that raises is reporting the caller's own bug — a broken
    /// socket, a full disk — and they catch it by class. [`Error`] has no
    /// variant that can carry a Python class, so the exception itself is kept
    /// here and re-raised at the `run()` boundary; what travels through the
    /// framework is only the summary that stops the run.
    ///
    /// [`Error`]: kerness::error::Error
    parked: Mutex<Option<PyErr>>,
}

impl PyChannel {
    /// Call *method* on the channel, keeping any exception for `run()`.
    fn call<A>(&self, method: &str, arguments: A) -> Result<()>
    where
        A: for<'py> IntoPyObject<'py, Target = pyo3::types::PyTuple>,
    {
        Python::with_gil(|py| {
            self.inner
                .bind(py)
                .call_method1(method, arguments)
                .map(drop)
                .inspect_err(|error| {
                    let mut parked = self.parked.lock().expect("channel park poisoned");
                    parked.get_or_insert_with(|| error.clone_ref(py));
                })
        })
        .catch()
    }

    /// Hand back the first exception a delivery raised, if there was one.
    pub fn parked(&self) -> Option<PyErr> {
        self.parked.lock().expect("channel park poisoned").take()
    }
}

impl Channel for PyChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        self.call("send", (sender, message))
    }

    fn send_system(&self, message: &str) -> Result<()> {
        self.call("send_system", (message,))
    }

    fn type_name(&self) -> String {
        self.name.clone()
    }
}

/// Wrap a Python channel so the framework can write to it.
///
/// `None` for a `None` object, so a session with no channel stays silent.
pub fn bind_channel(object: &Bound<'_, PyAny>) -> PyResult<Option<Arc<PyChannel>>> {
    if object.is_none() {
        return Ok(None);
    }
    Ok(Some(Arc::new(PyChannel {
        inner: object.clone().unbind(),
        name: object.get_type().name()?.to_string(),
        parked: Mutex::new(None),
    })))
}

/// Sends the framework's diagnostics to Python's `logging`.
///
/// Every record goes to the `kerness` logger, so a caller's existing handlers —
/// and `caplog` — see them without the framework holding a handler of its own.
struct PyLogger;

impl Logger for PyLogger {
    fn log(&self, level: Level, message: &str) {
        let method = match level {
            Level::Debug => "debug",
            Level::Warning => "warning",
            Level::Error => "error",
        };
        // A diagnostic that cannot be delivered is dropped: it is already the
        // report of something the framework survived, and raising here would
        // turn a survivable problem into a failed run.
        let _ = Python::with_gil(|py| -> PyResult<()> {
            py.import("logging")?
                .call_method1("getLogger", ("kerness",))?
                .call_method1(method, (message,))?;
            Ok(())
        });
    }
}

/// Route the framework's diagnostics through Python's `logging`.
pub fn install_logger() {
    kerness::logging::set_logger(Arc::new(PyLogger));
}
