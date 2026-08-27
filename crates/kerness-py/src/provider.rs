//! The provider seam.
//!
//! `Provider` is a Python abstract base class, because that is what callers
//! subclass and what `isinstance` has to agree with. Everything it does is
//! Rust: each instance owns a [`PyProviderCore`], and the base class's methods
//! are one-line forwards that hand `self` back down so the supplied logic —
//! the retry budget, the empty-reply guard, the one-way degrade latch, the
//! three-tier dialect choice — runs here while still calling back out to
//! whichever `chat` the subclass actually defined.
//!
//! A core built for one of the four backends carries the Rust provider that
//! speaks it, and shares that provider's degrade latch rather than keeping a
//! second copy: a 400 the endpoint returns has to be visible to the code that
//! builds the next payload.

use std::sync::Arc;

use kerness::error::{Error, Result};
use kerness::http::{self, Headers, HttpTransport, UreqTransport};
use kerness::provider::{
    supplied_chat_dispatch, supplied_chat_with_retries, supplied_effective_dialect,
    supplied_note_native_tools_rejected, ClaudeConfig, ClaudeCredential, ClaudeProvider,
    CustomConfig, CustomProvider, OpenAiConfig, OpenAiProvider, OpenRouterConfig,
    OpenRouterProvider, Provider, ProviderBase, ProviderResponse, CLAUDE_BASE_URL, OPENAI_BASE_URL,
    OPENROUTER_BASE_URL,
};
use kerness::tooling::ToolSpec;
use kerness::toolschema::ToolDialect;
use pyo3::exceptions::PyBaseException;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::convert::{optional_map, value_from_py, value_to_py};
use crate::errors::{from_py, to_py, Catch, Raise};
use crate::types::{
    dialect_from_py, dialect_to_py, messages_from_py, messages_to_py, optional_headers, tool_specs,
    tool_specs_to_py, PyProviderResponse,
};

/// Where the shared provider state lives.
///
/// Two cases, and the difference is which object owns the degrade latch: a
/// hand-written Python provider has no Rust half, so the core keeps the latch
/// itself; one of the four backends has a Rust provider that reads the latch
/// while building its payload, so the core points at that one instead of
/// keeping a second.
#[derive(Clone)]
enum Backing {
    Standalone(Arc<ProviderBase>),
    Backend(Arc<dyn Provider>),
}

impl Backing {
    fn base(&self) -> &ProviderBase {
        match self {
            Backing::Standalone(base) => base,
            Backing::Backend(provider) => provider.base(),
        }
    }
}

/// The Rust half of a `kerness.provider.Provider`.
#[pyclass(name = "ProviderCore", module = "kerness._core", frozen)]
pub struct PyProviderCore {
    backing: Backing,
}

impl PyProviderCore {
    fn backend(provider: impl Provider + 'static) -> Self {
        PyProviderCore {
            backing: Backing::Backend(Arc::new(provider)),
        }
    }

    /// The Rust backend, or a failure naming what was asked of a core that has
    /// none — an abstract `Provider` has no request to send.
    fn provider(&self) -> PyResult<Arc<dyn Provider>> {
        match &self.backing {
            Backing::Backend(provider) => Ok(Arc::clone(provider)),
            Backing::Standalone(_) => Err(to_py(Error::session(
                "This provider has no built-in backend; override chat().",
            ))),
        }
    }

    /// A view of *owner* the supplied methods can run against.
    fn view(&self, owner: &Bound<'_, PyAny>) -> PyResult<PyProvider> {
        PyProvider::new(owner, self.backing.clone())
    }
}

#[pymethods]
impl PyProviderCore {
    /// A core with no backend, for a provider whose `chat` is written in
    /// Python.
    #[new]
    #[pyo3(signature = (retries=2, backoff_sec=2.0, interval_sec=None))]
    fn new(retries: u32, backoff_sec: f64, interval_sec: Option<f64>) -> Self {
        PyProviderCore {
            backing: Backing::Standalone(Arc::new(ProviderBase::new(
                retries,
                backoff_sec,
                interval_sec,
            ))),
        }
    }

    #[staticmethod]
    #[pyo3(signature = (
        api_key,
        base_url=OPENROUTER_BASE_URL.to_string(),
        timeout_sec=60,
        retries=2,
        backoff_sec=2.0,
        interval_sec=None,
        temperature=1.0,
        top_p=1.0,
        max_tokens=None,
        app_url=String::new(),
        app_name=String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn openrouter(
        api_key: String,
        base_url: String,
        timeout_sec: u64,
        retries: u32,
        backoff_sec: f64,
        interval_sec: Option<f64>,
        temperature: f64,
        top_p: f64,
        max_tokens: Option<i64>,
        app_url: String,
        app_name: String,
    ) -> Self {
        PyProviderCore::backend(OpenRouterProvider::new(OpenRouterConfig {
            api_key,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            top_p,
            max_tokens,
            app_url,
            app_name,
        }))
    }

    #[staticmethod]
    #[pyo3(signature = (
        api_key,
        base_url=OPENAI_BASE_URL.to_string(),
        timeout_sec=60,
        retries=2,
        backoff_sec=2.0,
        interval_sec=None,
        temperature=1.0,
        top_p=1.0,
        max_tokens=None,
        output_schema=None,
        strict_json_schema=true,
        output_schema_name=String::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn openai(
        api_key: String,
        base_url: String,
        timeout_sec: u64,
        retries: u32,
        backoff_sec: f64,
        interval_sec: Option<f64>,
        temperature: f64,
        top_p: f64,
        max_tokens: Option<i64>,
        output_schema: Option<&Bound<'_, PyAny>>,
        strict_json_schema: bool,
        output_schema_name: String,
    ) -> PyResult<Self> {
        let output_schema = match output_schema.filter(|schema| !schema.is_none()) {
            Some(schema) => Some(value_from_py(schema)?),
            None => None,
        };
        Ok(PyProviderCore::backend(
            OpenAiProvider::new(OpenAiConfig {
                api_key,
                base_url,
                timeout_sec,
                retries,
                backoff_sec,
                interval_sec,
                temperature,
                top_p,
                max_tokens,
                output_schema,
                strict_json_schema,
                output_schema_name,
            })
            .raise()?,
        ))
    }

    /// *oauth* picks the credential header; everything else is identical, which
    /// is why the two Python classes share one backend.
    #[staticmethod]
    #[pyo3(signature = (
        api_key,
        base_url=CLAUDE_BASE_URL.to_string(),
        timeout_sec=60,
        retries=2,
        backoff_sec=2.0,
        interval_sec=None,
        temperature=1.0,
        max_tokens=4096,
        oauth=false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn claude(
        api_key: String,
        base_url: String,
        timeout_sec: u64,
        retries: u32,
        backoff_sec: f64,
        interval_sec: Option<f64>,
        temperature: f64,
        max_tokens: i64,
        oauth: bool,
    ) -> Self {
        let credential = if oauth {
            ClaudeCredential::OAuth(api_key)
        } else {
            ClaudeCredential::ApiKey(api_key)
        };
        PyProviderCore::backend(ClaudeProvider::new(ClaudeConfig {
            credential,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            max_tokens,
        }))
    }

    #[staticmethod]
    #[pyo3(signature = (
        url,
        api_key,
        model_config=None,
        timeout_sec=60,
        retries=2,
        backoff_sec=2.0,
        interval_sec=None,
        temperature=1.0,
        top_p=1.0,
        max_tokens=None,
        extra_headers=None,
        extra_body=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn custom(
        url: String,
        api_key: String,
        model_config: Option<&Bound<'_, PyAny>>,
        timeout_sec: u64,
        retries: u32,
        backoff_sec: f64,
        interval_sec: Option<f64>,
        temperature: f64,
        top_p: f64,
        max_tokens: Option<i64>,
        extra_headers: Option<&Bound<'_, PyAny>>,
        extra_body: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        Ok(PyProviderCore::backend(CustomProvider::new(CustomConfig {
            url,
            api_key,
            model_config: optional_map(model_config)?,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            top_p,
            max_tokens,
            extra_headers: optional_headers(extra_headers)?,
            extra_body: optional_map(extra_body)?,
        })))
    }

    /// Send one request through the Rust backend.
    #[pyo3(signature = (model, messages, tools=None))]
    fn chat(
        &self,
        model: &str,
        messages: &Bound<'_, PyAny>,
        tools: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyProviderResponse> {
        let provider = self.provider()?;
        let messages = messages_from_py(messages)?;
        let tools = tool_specs(tools)?;
        // The GIL is held throughout: the request goes out through a transport
        // that calls back into Python, so releasing it here would only mean
        // taking it again one frame down.
        let response = provider.chat(model, &messages, tools.as_deref());
        Ok(PyProviderResponse::adopt(response.raise()?))
    }

    /// The dialect to actually use, given what *owner* declares and answers.
    fn effective_dialect<'py>(
        &self,
        py: Python<'py>,
        owner: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        dialect_to_py(py, supplied_effective_dialect(&self.view(owner)?))
    }

    /// Latch *owner* down to the text protocol if *error* means "no tools".
    fn note_native_tools_rejected(
        &self,
        py: Python<'_>,
        owner: &Bound<'_, PyAny>,
        error: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        if !error.is_instance_of::<PyBaseException>() {
            return Ok(false);
        }
        let raised = PyErr::from_value(error.clone());
        Ok(supplied_note_native_tools_rejected(
            &self.view(owner)?,
            &from_py(py, &raised),
        ))
    }

    /// Send a request through *owner*, retrying on failure.
    #[pyo3(signature = (owner, model, messages, purpose="", tools=None))]
    fn chat_with_retries(
        &self,
        owner: &Bound<'_, PyAny>,
        model: &str,
        messages: &Bound<'_, PyAny>,
        purpose: &str,
        tools: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyProviderResponse> {
        let messages = messages_from_py(messages)?;
        let tools = tool_specs(tools)?;
        let response = supplied_chat_with_retries(
            &self.view(owner)?,
            model,
            &messages,
            purpose,
            tools.as_deref(),
        );
        Ok(PyProviderResponse::adopt(response.raise()?))
    }

    /// Call *owner*'s `chat`, passing *tools* only when they can be used.
    #[pyo3(signature = (owner, model, messages, tools=None))]
    fn chat_dispatch(
        &self,
        owner: &Bound<'_, PyAny>,
        model: &str,
        messages: &Bound<'_, PyAny>,
        tools: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyProviderResponse> {
        let messages = messages_from_py(messages)?;
        let tools = tool_specs(tools)?;
        let response =
            supplied_chat_dispatch(&self.view(owner)?, model, &messages, tools.as_deref());
        Ok(PyProviderResponse::adopt(response.raise()?))
    }
}

/// A Python provider, seen as a framework [`Provider`].
///
/// Every method routes back to the Python object, including the four the trait
/// supplies: a subclass that overrode `chat_with_retries` — which the test
/// doubles in this project's suite do — has to be the one that runs. The
/// supplied bodies are still reachable, because the base class's version of
/// each forwards into [`PyProviderCore`], which runs the free function rather
/// than the trait method it stands in for.
pub struct PyProvider {
    owner: Py<PyAny>,
    /// `type(owner).__name__`, read once: [`Provider::name`] hands out a
    /// borrow, and a name computed under the GIL could not outlive it.
    name: String,
    backing: Backing,
}

impl PyProvider {
    fn new(owner: &Bound<'_, PyAny>, backing: Backing) -> PyResult<Self> {
        Ok(PyProvider {
            owner: owner.clone().unbind(),
            name: owner.get_type().name()?.to_string(),
            backing,
        })
    }
}

impl Provider for PyProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn base(&self) -> &ProviderBase {
        self.backing.base()
    }

    fn chat(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        Python::with_gil(|py| {
            let owner = self.owner.bind(py);
            let messages = messages_to_py(py, messages)?;
            // `tools` goes as a keyword, and only when there is something to
            // send: a subclass whose `chat` predates tool calling never
            // declared the parameter, and never gets far enough to be offered
            // it either.
            let reply = match tools.filter(|tools| !tools.is_empty()) {
                Some(tools) => {
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("tools", tool_specs_to_py(py, tools)?)?;
                    owner.call_method("chat", (model, messages), Some(&kwargs))?
                }
                None => owner.call_method1("chat", (model, messages))?,
            };
            Ok(reply.extract::<PyProviderResponse>()?.inner)
        })
        .catch()
    }

    fn tool_dialect(&self) -> ToolDialect {
        Python::with_gil(|py| {
            self.owner
                .bind(py)
                .getattr("tool_dialect")
                .and_then(|value| dialect_from_py(&value))
        })
        .unwrap_or(ToolDialect::Text)
    }

    fn accepts_tools(&self) -> bool {
        Python::with_gil(|py| {
            self.owner
                .bind(py)
                .call_method0("_chat_accepts_tools")
                .and_then(|value| value.extract::<bool>())
        })
        .unwrap_or(false)
    }

    fn effective_dialect(&self) -> ToolDialect {
        Python::with_gil(|py| {
            self.owner
                .bind(py)
                .call_method0("effective_dialect")
                .and_then(|value| dialect_from_py(&value))
        })
        .unwrap_or(ToolDialect::Text)
    }

    fn note_native_tools_rejected(&self, error: &Error) -> bool {
        Python::with_gil(|py| {
            let raised = to_py(error.clone()).into_value(py);
            self.owner
                .bind(py)
                .call_method1("note_native_tools_rejected", (raised,))
                .and_then(|value| value.extract::<bool>())
        })
        .unwrap_or(false)
    }

    fn chat_with_retries(
        &self,
        model: &str,
        messages: &[Value],
        purpose: &str,
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        Python::with_gil(|py| {
            let kwargs = PyDict::new(py);
            kwargs.set_item("purpose", purpose)?;
            if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
                kwargs.set_item("tools", tool_specs_to_py(py, tools)?)?;
            }
            let messages = messages_to_py(py, messages)?;
            let reply = self.owner.bind(py).call_method(
                "chat_with_retries",
                (model, messages),
                Some(&kwargs),
            )?;
            Ok(reply.extract::<PyProviderResponse>()?.inner)
        })
        .catch()
    }

    fn chat_dispatch(
        &self,
        model: &str,
        messages: &[Value],
        tools: Option<&[ToolSpec]>,
    ) -> Result<ProviderResponse> {
        Python::with_gil(|py| {
            let messages = messages_to_py(py, messages)?;
            let tools = match tools.filter(|tools| !tools.is_empty()) {
                Some(tools) => tool_specs_to_py(py, tools)?.into_any(),
                None => py.None().into_bound(py),
            };
            let reply = self
                .owner
                .bind(py)
                .call_method1("_chat_dispatch", (model, messages, tools))?;
            Ok(reply.extract::<PyProviderResponse>()?.inner)
        })
        .catch()
    }
}

/// Wrap a Python provider so the framework can call it.
///
/// `None` for a `None` object, so an agent with no provider stays one.
pub fn bind_provider(object: &Bound<'_, PyAny>) -> PyResult<Option<Arc<dyn Provider>>> {
    if object.is_none() {
        return Ok(None);
    }
    let backing = match object.getattr("_core") {
        Ok(core) => core.extract::<PyRef<'_, PyProviderCore>>()?.backing.clone(),
        // A provider that never called `Provider.__init__` still has to be
        // callable; it just gets the default retry budget rather than one it
        // never chose.
        Err(_) => Backing::Standalone(Arc::new(ProviderBase::default())),
    };
    Ok(Some(Arc::new(PyProvider::new(object, backing)?)))
}

/// Lift the system messages out of *messages*, as the Claude API wants them.
///
/// The Claude API takes `system` as a top-level parameter rather than as a
/// message, so the two come back separately.
#[pyfunction]
#[pyo3(name = "_convert_messages_for_claude")]
pub fn convert_messages_for_claude<'py>(
    py: Python<'py>,
    messages: &Bound<'py, PyAny>,
) -> PyResult<(String, Bound<'py, pyo3::types::PyList>)> {
    let (system, filtered) =
        kerness::provider::convert_messages_for_claude(&messages_from_py(messages)?);
    Ok((system, messages_to_py(py, &filtered)?))
}

// ------------------------------------------------------------ HTTP transport

/// A transport that routes every built-in backend through the Python name
/// `kerness.provider.http_post_json`.
///
/// Resolved at call time rather than at install time, which is the whole
/// point: a test that patches that attribute intercepts the request without
/// any provider knowing there was something to intercept.
struct PyTransport;

impl HttpTransport for PyTransport {
    fn post_json(
        &self,
        url: &str,
        payload: &Value,
        headers: &Headers,
        timeout_sec: u64,
    ) -> Result<Value> {
        Python::with_gil(|py| {
            let header_dict = PyDict::new(py);
            for (name, value) in headers {
                header_dict.set_item(name, value)?;
            }
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", timeout_sec)?;
            let response = py
                .import("kerness.provider")?
                .getattr("http_post_json")?
                .call(
                    (url, value_to_py(py, payload)?, header_dict),
                    Some(&kwargs),
                )?;
            value_from_py(&response)
        })
        .catch()
    }
}

/// Route the built-in backends through Python's `http_post_json`.
pub fn install_transport() {
    http::set_transport(Arc::new(PyTransport));
}

/// Send a JSON POST and return the parsed body.
///
/// The unpatched implementation behind `kerness.provider.http_post_json`,
/// calling the default transport directly — going back through the installed
/// one would be this function calling itself.
#[pyfunction]
#[pyo3(signature = (url, payload, headers=None, timeout=60))]
pub fn http_post_json<'py>(
    py: Python<'py>,
    url: &str,
    payload: &Bound<'py, PyAny>,
    headers: Option<&Bound<'py, PyAny>>,
    timeout: u64,
) -> PyResult<Bound<'py, PyAny>> {
    let payload = value_from_py(payload)?;
    let headers = optional_headers(headers)?;
    let response = py
        .allow_threads(|| UreqTransport.post_json(url, &payload, &headers, timeout))
        .raise()?;
    value_to_py(py, &response)
}
