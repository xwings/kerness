//! Where a session keeps what its agents remember.
//!
//! The default store is the crate's — [`kerness::memory::FileMemory`] decides
//! how a note is separated from the one before it, what an absent file reads
//! as, and which scope an agent addresses — and what lives here is only the
//! wrapper that lets a caller pass it, subclass around it, or replace it.
//!
//! The other direction is here too: a store the caller wrote in Python, seen by
//! the framework as a [`MemoryStore`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kerness::error::Result;
use kerness::memory::{FileMemory, MemoryStore};
use kerness::session::Memories;
use pyo3::prelude::*;

use crate::errors::{Catch, Raise};
use crate::types::path_to_py;

/// A Python memory store, seen as a framework [`MemoryStore`].
///
/// Every method is fallible on both sides, so an exception the store raises
/// travels back out of `run()` as the same class it was raised as — there is
/// nothing to park, unlike a channel, whose `send` cannot fail.
pub struct PyStore {
    inner: Py<PyAny>,
}

impl PyStore {
    /// Read an optional attribute that the trait's own default answers `None`
    /// for, treating a raise as that same `None`.
    ///
    /// The trait cannot report a failure here — `age` and `path` return
    /// `Option`, not `Result` — and the honest reading of a store that cannot
    /// name its file is a store that names none, which is what a store keeping
    /// nothing on disk answers anyway. Logged, because it is a bug in the
    /// store and silence would hide it.
    fn optional<T>(&self, method: &str, scope: &str) -> Option<T>
    where
        T: for<'py> FromPyObject<'py>,
    {
        Python::with_gil(|py| {
            match self.inner.bind(py).call_method1(method, (scope,)) {
                Ok(value) if value.is_none() => None,
                Ok(value) => match value.extract::<T>() {
                    Ok(value) => Some(value),
                    Err(error) => {
                        kerness::logging::warning(&format!(
                            "memory store {method}({scope}) returned something \
                             unusable; treating it as None: {error}"
                        ));
                        None
                    }
                },
                Err(error) => {
                    kerness::logging::warning(&format!(
                        "memory store {method}({scope}) raised; treating it as \
                         None: {error}"
                    ));
                    None
                }
            }
        })
    }
}

impl MemoryStore for PyStore {
    fn read(&self, scope: &str) -> Result<String> {
        Python::with_gil(|py| {
            let value = self.inner.bind(py).call_method1("read", (scope,))?;
            Ok(value.str()?.to_string_lossy().into_owned())
        })
        .catch()
    }

    fn append(&self, scope: &str, note: &str) -> Result<()> {
        Python::with_gil(|py| {
            self.inner
                .bind(py)
                .call_method1("append", (scope, note))
                .map(drop)
        })
        .catch()
    }

    fn open(&self, scope: &str) -> Result<()> {
        Python::with_gil(|py| self.inner.bind(py).call_method1("open", (scope,)).map(drop)).catch()
    }

    fn age(&self, scope: &str) -> Option<u64> {
        self.optional("age", scope)
    }

    fn path(&self, scope: &str) -> Option<PathBuf> {
        self.optional("path", scope)
    }

    fn close(&self) -> Result<()> {
        Python::with_gil(|py| self.inner.bind(py).call_method0("close").map(drop)).catch()
    }
}

/// Bind *object* so the session can keep its memory in it.
///
/// `None` for a `None` object, so a session that names no store gets the
/// crate's default.
pub fn bind_memory_store(object: &Bound<'_, PyAny>) -> PyResult<Option<Arc<dyn MemoryStore>>> {
    if object.is_none() {
        return Ok(None);
    }
    // Exact type only, for the reason `native_channel` takes the same care: a
    // subclass overriding `read` is a caller's store that happens to inherit,
    // and the shortcut past it would call the base the subclass exists to wrap.
    if let Ok(native) = object.downcast_exact::<PyFileMemory>() {
        return Ok(Some(native.get().inner.clone()));
    }
    Ok(Some(Arc::new(PyStore {
        inner: object.clone().unbind(),
    })))
}

/// Keeps each scope in a Markdown file of its own.
#[pyclass(name = "FileMemory", module = "kerness._core", frozen, subclass)]
pub struct PyFileMemory {
    inner: Arc<dyn MemoryStore>,
}

#[pymethods]
impl PyFileMemory {
    #[new]
    fn new() -> Self {
        PyFileMemory {
            inner: Arc::new(FileMemory::new()),
        }
    }

    fn read(&self, scope: &str) -> PyResult<String> {
        self.inner.read(scope).raise()
    }

    fn append(&self, scope: &str, note: &str) -> PyResult<()> {
        self.inner.append(scope, note).raise()
    }

    fn open(&self, scope: &str) -> PyResult<()> {
        self.inner.open(scope).raise()
    }

    fn age(&self, scope: &str) -> Option<u64> {
        self.inner.age(scope)
    }

    fn path(&self, py: Python<'_>, scope: &str) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.path(scope) {
            Some(path) => Ok(Some(path_to_py(py, path.to_string_lossy().as_ref())?)),
            None => Ok(None),
        }
    }

    fn close(&self) -> PyResult<()> {
        self.inner.close().raise()
    }
}

/// A running session's memory, at its session-level scope.
///
/// Live rather than a snapshot: the session writes during the run, so a copy
/// taken when the attribute was first read would be stale by the time `run()`
/// returned.
#[pyclass(name = "SessionMemory", module = "kerness._core")]
pub struct PySessionMemory {
    memories: Arc<Mutex<Memories>>,
}

impl PySessionMemory {
    pub fn of_session(memories: Arc<Mutex<Memories>>) -> Self {
        PySessionMemory { memories }
    }

    /// The store and the session scope, taken together and under one lock.
    fn store(&self) -> (Arc<dyn MemoryStore>, String) {
        let memories = self
            .memories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            Arc::clone(&memories.store),
            memories.session_scope.clone(),
        )
    }
}

#[pymethods]
impl PySessionMemory {
    /// The scope the session addresses its store by.
    #[getter]
    fn scope(&self) -> String {
        self.store().1
    }

    /// Everything stored at the session scope, as the prompt quotes it.
    fn read(&self) -> PyResult<String> {
        let (store, scope) = self.store();
        store.read(&scope).raise()
    }

    /// Store *note* at the session scope, as its own entry.
    ///
    /// The session's own memory filter is not consulted: it gates what *agents*
    /// write, and a caller reaching this attribute is the one who installed it.
    fn append(&self, note: &str) -> PyResult<()> {
        let (store, scope) = self.store();
        store.append(&scope, note).raise()
    }

    /// The file the store keeps this scope in, or ``None`` when it keeps none.
    #[getter]
    fn path(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let (store, scope) = self.store();
        match store.path(&scope) {
            Some(path) => Ok(Some(path_to_py(py, path.to_string_lossy().as_ref())?)),
            None => Ok(None),
        }
    }

    /// Whole days since the scope was last written, or ``None`` when the store
    /// cannot date it.
    #[getter]
    fn age(&self) -> Option<u64> {
        let (store, scope) = self.store();
        store.age(&scope)
    }
}
