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
            Ok(response) => serde_json::from_reader(response.into_reader()).map_err(|err| {
                if err.is_io() {
                    Error::ProviderNetwork {
                        url: url.to_string(),
                        cause: err.to_string(),
                    }
                } else {
                    Error::provider(format!("Invalid JSON response from {url}: {err}"))
                }
            }),
            Err(ureq::Error::Status(status, response)) => Err(Error::ProviderHttp {
                status_code: status,
                url: url.to_string(),
                body: response
                    .into_string()
                    .unwrap_or_else(|err| format!("Could not read error response body: {err}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    #[test]
    fn response_errors_are_distinct_from_network_failures() {
        for (status, body, declared_length) in [
            (200, b"{\"ok\":true}".as_slice(), None),
            (200, b"not json", None),
            (200, b"{\"text\":\"\xff\"}", None),
            (200, b"{", Some(100)),
            (429, b"{\"error\":\"rate limited\"}", None),
            (403, b"blocked", Some(100)),
            (0, b"", None),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert_ne!(request.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                request.read_exact(&mut vec![0; length]).unwrap();
                if status != 0 {
                    let length = declared_length.unwrap_or(body.len());
                    write!(socket, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
                    socket.write_all(body).unwrap();
                }
            });
            let result = UreqTransport.post_json(&url, &serde_json::json!({}), &vec![], 5);
            server.join().unwrap();
            match (status, declared_length) {
                (200, None) if body != b"{\"ok\":true}" => {
                    assert!(matches!(result, Err(Error::Provider(_))), "{result:?}");
                    let message = result.unwrap_err().to_string();
                    assert!(message.contains("Invalid JSON response"), "{message}");
                    assert!(message.contains(&url), "{message}");
                }
                (200, None) => assert_eq!(result.unwrap(), serde_json::json!({"ok": true})),
                (200, Some(_)) | (0, _) => {
                    assert!(matches!(result, Err(Error::ProviderNetwork { .. })))
                }
                _ => {
                    let Error::ProviderHttp {
                        status_code,
                        url: actual_url,
                        body: actual_body,
                    } = result.unwrap_err()
                    else {
                        panic!("expected HTTP error");
                    };
                    assert_eq!(status_code, status);
                    assert_eq!(actual_url, url);
                    if status == 403 {
                        assert!(
                            actual_body.contains("Could not read error response body"),
                            "{actual_body}"
                        );
                    } else {
                        assert_eq!(actual_body.as_bytes(), body);
                    }
                }
            }
        }
    }
}
