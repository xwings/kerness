//! The framework's error type.
//!
//! One flat enum, which the Python bindings fan back out into an exception
//! class per variant.
//! The hierarchy is only two levels deep and callers branch on exactly one
//! relationship — "is this a provider error?" — so [`Error::is_provider`]
//! carries what a nested enum would have cost a type to express.

use std::fmt;

/// Every way a run can fail.
///
/// `Display` is the message a caller reads, and the bindings surface each
/// variant as its own exception class — so the split drawn here is the split
/// every caller sees, in either language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A provider failed in a way none of the specific variants describe.
    Provider(String),
    /// The provider answered with a non-2xx status.
    ProviderHttp {
        status_code: u16,
        url: String,
        body: String,
    },
    /// The provider could not be reached at all.
    ProviderNetwork { url: String, cause: String },
    /// The provider answered with neither content nor tool calls.
    ProviderEmpty(String),
    /// The session cannot proceed: a broken contract, limit, or configuration.
    Session(String),
    /// A gameplan file could not be read or parsed.
    GameplanLoad(String),
    /// The access policy refused a command or path.
    AccessDenied(String),
    /// A required file is absent. Surfaced as Python's `FileNotFoundError`,
    /// which is what the loaders raise rather than a framework exception —
    /// a missing persona or skill file is the caller's typo, not a broken
    /// contract.
    NotFound(String),
    /// A file the caller wrote is malformed. Surfaced as Python's `ValueError`,
    /// which is what the skill loader and agent validation raise rather than a
    /// framework exception — a bad skill name is the author's typo, not a
    /// broken session contract.
    Value(String),
    /// Any other filesystem failure. Surfaced as Python's `OSError`.
    Io(String),
}

impl Error {
    /// Whether this is one of the provider variants.
    ///
    /// The retry and dialect-fallback paths catch provider failures and let
    /// everything else through, which is the only place the exception
    /// hierarchy's shape is load-bearing.
    pub fn is_provider(&self) -> bool {
        matches!(
            self,
            Error::Provider(_)
                | Error::ProviderHttp { .. }
                | Error::ProviderNetwork { .. }
                | Error::ProviderEmpty(_)
        )
    }

    /// Shorthand for [`Error::Session`] over anything string-like.
    pub fn session(message: impl Into<String>) -> Self {
        Error::Session(message.into())
    }

    /// Shorthand for [`Error::Provider`] over anything string-like.
    pub fn provider(message: impl Into<String>) -> Self {
        Error::Provider(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Provider(msg)
            | Error::ProviderEmpty(msg)
            | Error::Session(msg)
            | Error::GameplanLoad(msg)
            | Error::AccessDenied(msg)
            | Error::NotFound(msg)
            | Error::Value(msg)
            | Error::Io(msg) => f.write_str(msg),
            Error::ProviderHttp {
                status_code,
                url,
                body,
            } => write!(f, "HTTP {status_code} from {url}: {body}"),
            Error::ProviderNetwork { url, cause } => {
                write!(f, "Network error for {url}: {cause}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;
