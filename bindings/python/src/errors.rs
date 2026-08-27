//! The exception classes, and the two-way map between them and [`Error`].
//!
//! The classes themselves are written in Python, in `kerness.exceptions`, and
//! handed down here at import. Two of them take more than a message —
//! `ProviderHTTPError(status_code, url, body)` and `ProviderNetworkError(url,
//! cause)` keep those as attributes — and a Python `__init__` says so in one
//! line each. The map has to run in both directions because a provider written
//! in Python raises into Rust code that reads the status code off the failure
//! to decide whether to stop offering tool schemas.

use std::sync::OnceLock;

use kerness::error::Error;
use pyo3::exceptions::{PyFileNotFoundError, PyOSError, PyValueError};
use pyo3::prelude::*;

/// The classes `kerness.exceptions` defines.
struct Classes {
    session: Py<PyAny>,
    gameplan_load: Py<PyAny>,
    access_denied: Py<PyAny>,
    provider: Py<PyAny>,
    provider_http: Py<PyAny>,
    provider_network: Py<PyAny>,
    provider_empty: Py<PyAny>,
}

static CLASSES: OnceLock<Classes> = OnceLock::new();

/// Record the exception classes from an already-imported `kerness.exceptions`.
pub fn register(module: &Bound<'_, PyAny>) -> PyResult<()> {
    let class = |name: &str| -> PyResult<Py<PyAny>> { Ok(module.getattr(name)?.unbind()) };
    let classes = Classes {
        session: class("SessionError")?,
        gameplan_load: class("GameplanLoadError")?,
        access_denied: class("AccessDeniedError")?,
        provider: class("ProviderError")?,
        provider_http: class("ProviderHTTPError")?,
        provider_network: class("ProviderNetworkError")?,
        provider_empty: class("ProviderEmptyResponseError")?,
    };
    // Re-importing the package re-runs the call; the first set is the one that
    // matters, and the classes are identical either way.
    let _ = CLASSES.set(classes);
    Ok(())
}

fn classes() -> &'static Classes {
    CLASSES
        .get()
        .expect("kerness.exceptions has not been registered with kerness._core")
}

/// Raise *error* as the Python class that stands for its variant.
pub fn to_py(error: Error) -> PyErr {
    Python::with_gil(|py| {
        let known = classes();
        let built = match &error {
            Error::Provider(message) => known.provider.bind(py).call1((message,)),
            Error::ProviderEmpty(message) => known.provider_empty.bind(py).call1((message,)),
            Error::Session(message) => known.session.bind(py).call1((message,)),
            Error::GameplanLoad(message) => known.gameplan_load.bind(py).call1((message,)),
            Error::AccessDenied(message) => known.access_denied.bind(py).call1((message,)),
            Error::ProviderHttp {
                status_code,
                url,
                body,
            } => known
                .provider_http
                .bind(py)
                .call1((*status_code, url, body)),
            Error::ProviderNetwork { url, cause } => {
                known.provider_network.bind(py).call1((url, cause))
            }
            Error::NotFound(message) => return PyFileNotFoundError::new_err(message.clone()),
            Error::Value(message) => return PyValueError::new_err(message.clone()),
            Error::Io(message) => return PyOSError::new_err(message.clone()),
        };
        match built {
            Ok(instance) => PyErr::from_value(instance),
            Err(failure) => failure,
        }
    })
}

/// Read a Python exception back as the variant it stands for.
///
/// Anything unrecognized becomes [`Error::Session`], which carries the message
/// and is not a provider failure — so a test double that raises `RuntimeError`
/// spends the retry budget and is then reported, exactly as one raising
/// `SessionError` would be.
pub fn from_py(py: Python<'_>, error: &PyErr) -> Error {
    let value = error.value(py);
    let message = value
        .str()
        .map(|text| text.to_string_lossy().into_owned())
        .unwrap_or_default();
    let known = classes();
    let is = |class: &Py<PyAny>| value.is_instance(class.bind(py)).unwrap_or(false);
    let attribute = |name: &str| -> String {
        value
            .getattr(name)
            .and_then(|found| found.str())
            .map(|text| text.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    if is(&known.provider_http) {
        return Error::ProviderHttp {
            status_code: value
                .getattr("status_code")
                .and_then(|found| found.extract::<u16>())
                .unwrap_or_default(),
            url: attribute("url"),
            body: attribute("body"),
        };
    }
    if is(&known.provider_network) {
        return Error::ProviderNetwork {
            url: attribute("url"),
            cause: attribute("cause"),
        };
    }
    if is(&known.provider_empty) {
        return Error::ProviderEmpty(message);
    }
    if is(&known.provider) {
        return Error::Provider(message);
    }
    if is(&known.access_denied) {
        return Error::AccessDenied(message);
    }
    if is(&known.gameplan_load) {
        return Error::GameplanLoad(message);
    }
    if value.is_instance_of::<PyFileNotFoundError>() {
        return Error::NotFound(message);
    }
    if value.is_instance_of::<PyValueError>() {
        return Error::Value(message);
    }
    Error::Session(message)
}

/// Turn a framework result into a Python one.
pub trait Raise<T> {
    fn raise(self) -> PyResult<T>;
}

impl<T> Raise<T> for kerness::error::Result<T> {
    fn raise(self) -> PyResult<T> {
        self.map_err(to_py)
    }
}

/// Turn a Python result into a framework one.
pub trait Catch<T> {
    fn catch(self) -> kerness::error::Result<T>;
}

impl<T> Catch<T> for PyResult<T> {
    fn catch(self) -> kerness::error::Result<T> {
        self.map_err(|error| Python::with_gil(|py| from_py(py, &error)))
    }
}
