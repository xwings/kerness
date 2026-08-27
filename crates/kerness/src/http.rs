//! JSON-over-HTTP transport for the built-in providers.
//!
//! The transport is a process-global indirection rather than a field on each
//! provider. That is what makes it swappable from outside the crate: the
//! Python bindings install a transport that routes through the module-level
//! `http_post_json`, so a test that patches that name intercepts every
//! built-in provider without any of them knowing.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};

/// An ordered list of request headers.
///
/// Ordered because the bindings hand these to Python as a `dict`, and a caller
/// reading a captured request should see the headers the provider wrote in the
/// order it wrote them.
pub type Headers = Vec<(String, String)>;

/// Sends a JSON payload and returns the parsed JSON response.
pub trait HttpTransport: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        payload: &Value,
        headers: &Headers,
        timeout_sec: u64,
    ) -> Result<Value>;
}

/// The default transport: a blocking `ureq` call.
pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn post_json(
        &self,
        url: &str,
        payload: &Value,
        headers: &Headers,
        timeout_sec: u64,
    ) -> Result<Value> {
        let mut request = ureq::post(url).timeout(Duration::from_secs(timeout_sec));
        for (name, value) in headers {
            request = request.set(name, value);
        }
        match request.send_json(payload) {
            Ok(response) => response.into_json::<Value>().map_err(|err| Error::ProviderNetwork {
                url: url.to_string(),
                cause: err.to_string(),
            }),
            Err(ureq::Error::Status(status, response)) => Err(Error::ProviderHttp {
                status_code: status,
                url: url.to_string(),
                body: response.into_string().unwrap_or_default(),
            }),
            Err(ureq::Error::Transport(transport)) => Err(Error::ProviderNetwork {
                url: url.to_string(),
                cause: transport.to_string(),
            }),
        }
    }
}

fn slot() -> &'static RwLock<Arc<dyn HttpTransport>> {
    static SLOT: OnceLock<RwLock<Arc<dyn HttpTransport>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(UreqTransport)))
}

/// Replace the transport every built-in provider uses.
pub fn set_transport(transport: Arc<dyn HttpTransport>) {
    *slot().write().expect("transport lock poisoned") = transport;
}

/// Send a JSON POST through the current transport and return the parsed body.
pub fn post_json(url: &str, payload: &Value, headers: &Headers, timeout_sec: u64) -> Result<Value> {
    let transport = Arc::clone(&*slot().read().expect("transport lock poisoned"));
    transport.post_json(url, payload, headers, timeout_sec)
}
