//! Output channels, and where the framework's own diagnostics go.
//!
//! The four bundled channels are the crate's — [`kerness::channel`] composes
//! every line, picks every timestamp, and owns the fan-out's degrade rule — and
//! what lives here is only their delivery. Two seams carry that across: the
//! [`ConsoleWriter`] the crate consults instead of writing to file descriptor 1,
//! so a line reaches Python's `sys.stdout` and therefore a notebook cell and
//! `capsys`; and the [`Logger`] the crate consults instead of stderr, so a
//! failed delivery reaches the caller's `logging` handlers.
//!
//! The other direction is here too: a channel the caller wrote in Python, seen
//! by the framework as a [`Channel`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kerness::channel::{
    Channel, ConsoleChannel, ConsoleWriter, FileChannel, LogChannel, MultiChannel,
};
use kerness::error::Result;
use kerness::logging::{Level, Logger};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::errors::{Catch, Raise};
use crate::types::path_to_py;

/// A Python channel, seen as a framework [`Channel`].
pub struct PyChannel {
    inner: Py<PyAny>,
    /// `type(inner).__name__`, read once: [`Channel::type_name`] is only ever
    /// read by a failure log, and it has to name something the caller can go
    /// and fix.
    name: String,
    /// `inner.paths()`, read once: [`Channel::paths`] is read by
    /// `Session::new` to confine the channel's destination to the access workspace,
    /// and reading it there instead would leave a `PyErr` nowhere to go —
    /// `paths` cannot fail, so the extraction has to happen where a raising
    /// `paths()` can still reach the caller as an exception.
    paths: Vec<PathBuf>,
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

    fn paths(&self) -> Vec<PathBuf> {
        self.paths.clone()
    }
}

/// A channel the session can write to, and the Python object behind it if there
/// is one.
///
/// The two are separate because only a channel the *caller* wrote can park an
/// exception for `run()` to re-raise. A bundled channel is the crate's own code
/// and reports through `Result` like everything else in it.
pub struct BoundChannel {
    /// What the session writes to.
    pub channel: Arc<dyn Channel>,
    /// The same channel, when it is a Python object whose `parked()` has to be
    /// drained at the `run()` boundary.
    pub python: Option<Arc<PyChannel>>,
}

/// Bind *object* so the framework can write to it.
///
/// `None` for a `None` object, so a session with no channel stays silent.
pub fn bind_channel(object: &Bound<'_, PyAny>) -> PyResult<Option<BoundChannel>> {
    if object.is_none() {
        return Ok(None);
    }
    if let Some(channel) = native_channel(object) {
        return Ok(Some(BoundChannel {
            channel,
            python: None,
        }));
    }
    // A channel the caller wrote against an older base class, or duck-typed
    // without inheriting at all, has no `paths` — the same answer the Rust
    // trait's default gives, and for the same reason: a channel that names no
    // file has none for the workspace to confine.
    let paths = match object.getattr("paths") {
        Ok(paths) => paths.call0()?.extract::<Vec<PathBuf>>()?,
        Err(_) => Vec::new(),
    };
    let python = Arc::new(PyChannel {
        inner: object.clone().unbind(),
        name: object.get_type().name()?.to_string(),
        paths,
        parked: Mutex::new(None),
    });
    Ok(Some(BoundChannel {
        channel: python.clone(),
        python: Some(python),
    }))
}

/// The crate's own channel behind *object*, if that is what it is.
///
/// A bundled channel wrapped in [`PyChannel`] would work, and would cost a GIL
/// acquisition and a Python method dispatch per line to arrive back at the Rust
/// call it started from.
///
/// Exact types only. A subclass overriding `send` is a caller's channel that
/// happens to inherit, and taking the shortcut past it would call the base
/// implementation the subclass exists to wrap.
fn native_channel(object: &Bound<'_, PyAny>) -> Option<Arc<dyn Channel>> {
    if let Ok(native) = object.downcast_exact::<PyConsoleChannel>() {
        return Some(native.get().inner.clone());
    }
    if let Ok(native) = object.downcast_exact::<PyFileChannel>() {
        return Some(native.get().inner.clone());
    }
    if let Ok(native) = object.downcast_exact::<PyLogChannel>() {
        return Some(native.get().inner.clone());
    }
    if let Ok(native) = object.downcast_exact::<PyMultiChannel>() {
        return Some(native.get().inner.clone());
    }
    None
}

/// The channel's `paths()` as `pathlib.Path` objects.
fn paths_of(py: Python<'_>, channel: &Arc<dyn Channel>) -> PyResult<Vec<Py<PyAny>>> {
    channel
        .paths()
        .iter()
        .map(|path| path_to_py(py, &path.to_string_lossy()))
        .collect()
}

/// Prints messages to `sys.stdout`.
#[pyclass(name = "ConsoleChannel", module = "kerness._core", frozen, subclass)]
pub struct PyConsoleChannel {
    inner: Arc<dyn Channel>,
}

#[pymethods]
impl PyConsoleChannel {
    #[new]
    #[pyo3(signature = (prefix_format = "[{sender}]".to_string()))]
    fn new(prefix_format: String) -> Self {
        PyConsoleChannel {
            inner: Arc::new(ConsoleChannel::new(prefix_format)),
        }
    }

    fn send(&self, sender: &str, message: &str) -> PyResult<()> {
        self.inner.send(sender, message).raise()
    }

    fn send_system(&self, message: &str) -> PyResult<()> {
        self.inner.send_system(message).raise()
    }

    fn paths(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        paths_of(py, &self.inner)
    }
}

/// Appends plain text to a file.
#[pyclass(name = "FileChannel", module = "kerness._core", frozen, subclass)]
pub struct PyFileChannel {
    inner: Arc<dyn Channel>,
}

#[pymethods]
impl PyFileChannel {
    #[new]
    fn new(filepath: PathBuf) -> Self {
        PyFileChannel {
            inner: Arc::new(FileChannel::new(filepath)),
        }
    }

    fn send(&self, sender: &str, message: &str) -> PyResult<()> {
        self.inner.send(sender, message).raise()
    }

    fn send_system(&self, message: &str) -> PyResult<()> {
        self.inner.send_system(message).raise()
    }

    fn paths(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        paths_of(py, &self.inner)
    }
}

/// Writes one JSON object per line to a timestamped file.
#[pyclass(name = "LogChannel", module = "kerness._core", frozen, subclass)]
pub struct PyLogChannel {
    inner: Arc<dyn Channel>,
}

#[pymethods]
impl PyLogChannel {
    #[new]
    #[pyo3(signature = (log_dir = PathBuf::from("logs")))]
    fn new(log_dir: PathBuf) -> PyResult<Self> {
        Ok(PyLogChannel {
            inner: Arc::new(LogChannel::new(log_dir).raise()?),
        })
    }

    fn send(&self, sender: &str, message: &str) -> PyResult<()> {
        self.inner.send(sender, message).raise()
    }

    fn send_system(&self, message: &str) -> PyResult<()> {
        self.inner.send_system(message).raise()
    }

    fn paths(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        paths_of(py, &self.inner)
    }
}

/// Fans out to several channels at once.
#[pyclass(name = "MultiChannel", module = "kerness._core", frozen, subclass)]
pub struct PyMultiChannel {
    inner: Arc<dyn Channel>,
}

#[pymethods]
impl PyMultiChannel {
    #[new]
    #[pyo3(signature = (*channels))]
    fn new(channels: &Bound<'_, PyTuple>) -> PyResult<Self> {
        let mut members: Vec<Arc<dyn Channel>> = Vec::with_capacity(channels.len());
        for channel in channels.iter() {
            // A member that raises is reported by the fan-out and the run goes
            // on, so its `parked()` is deliberately dropped: re-raising it at
            // `run()` would undo the degrade the fan-out just performed.
            match bind_channel(&channel)? {
                Some(bound) => members.push(bound.channel),
                None => {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "MultiChannel members must be channels, not None",
                    ))
                }
            }
        }
        Ok(PyMultiChannel {
            inner: Arc::new(MultiChannel::new(members)),
        })
    }

    fn send(&self, sender: &str, message: &str) -> PyResult<()> {
        self.inner.send(sender, message).raise()
    }

    fn send_system(&self, message: &str) -> PyResult<()> {
        self.inner.send_system(message).raise()
    }

    fn paths(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        paths_of(py, &self.inner)
    }
}

/// Sends [`ConsoleChannel`]'s lines to `sys.stdout`.
///
/// `print` rather than a write to file descriptor 1, because they are not the
/// same destination: a caller who replaced `sys.stdout`, a notebook cell, and
/// pytest's `capsys` all see the first and none of them see the second.
struct PyConsoleWriter;

impl ConsoleWriter for PyConsoleWriter {
    fn write_line(&self, line: &str) -> Result<()> {
        Python::with_gil(|py| -> PyResult<()> {
            let options = PyDict::new(py);
            options.set_item("flush", true)?;
            py.import("builtins")?
                .call_method("print", (line,), Some(&options))?;
            Ok(())
        })
        .catch()
    }
}

/// Route [`ConsoleChannel`]'s output through Python's `print`.
pub fn install_console_writer() {
    kerness::channel::set_console_writer(Arc::new(PyConsoleWriter));
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
