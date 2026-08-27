//! Where the framework's diagnostics go.
//!
//! The framework reports a handful of things it survived — a retry, a provider
//! failure it is about to retry, a channel that could not deliver, a
//! compaction summary that came back empty. None of them stop a run, and none
//! of them belong on stdout, which is the caller's transcript.
//!
//! A process-wide sink rather than a returned value or a `log` facade: the
//! callers are deep inside the loop and have nothing to hand a diagnostic to,
//! and the binding needs these records to arrive in Python's `logging` so that
//! a caller's existing handlers — and `caplog` — see them. The default writes
//! to stderr so the pure-Rust crate is useful without any wiring.

use std::sync::{Arc, OnceLock, RwLock};

/// How much the reader is expected to care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Routine detail; off in any normal configuration.
    Debug,
    /// Something went wrong and the framework worked around it.
    Warning,
    /// Something went wrong and the caller lost output because of it.
    Error,
}

/// A destination for diagnostics.
pub trait Logger: Send + Sync {
    fn log(&self, level: Level, message: &str);
}

/// The default: warnings and errors to stderr, debug dropped.
struct StderrLogger;

impl Logger for StderrLogger {
    fn log(&self, level: Level, message: &str) {
        match level {
            Level::Debug => {}
            Level::Warning => eprintln!("WARNING: {message}"),
            Level::Error => eprintln!("ERROR: {message}"),
        }
    }
}

fn slot() -> &'static RwLock<Arc<dyn Logger>> {
    static SLOT: OnceLock<RwLock<Arc<dyn Logger>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(StderrLogger)))
}

/// Send every later diagnostic to *logger*.
pub fn set_logger(logger: Arc<dyn Logger>) {
    *slot().write().expect("logger lock poisoned") = logger;
}

/// Record one diagnostic.
pub fn log(level: Level, message: &str) {
    let logger = Arc::clone(&*slot().read().expect("logger lock poisoned"));
    logger.log(level, message);
}

/// Record a [`Level::Debug`] diagnostic.
pub fn debug(message: &str) {
    log(Level::Debug, message);
}

/// Record a [`Level::Warning`] diagnostic.
pub fn warning(message: &str) {
    log(Level::Warning, message);
}

/// Record a [`Level::Error`] diagnostic.
pub fn error(message: &str) {
    log(Level::Error, message);
}
