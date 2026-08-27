//! Kerness — a synchronous framework for building multi-agent harnesses.
//!
//! A Markdown gameplan supplies a machine-readable harness contract in YAML
//! frontmatter and human instructions in its body. The contract controls roles,
//! participant bounds, loop limits and phases, termination tokens, available
//! tools and skills, and the returned result shape.
//!
//! Everything the harness sits on belongs to the framework — provider
//! transport, tool dialects, prompt assembly, the orchestrator loop, access
//! control, memory, session files, and compaction — so a new harness is a new
//! Markdown file rather than a new runtime.
//!
//! There is no daemon and no server: a run that is asked for a session file
//! writes its own state to disk after every turn and continues from that file
//! the next time the same program runs.

pub mod access;
pub mod agent;
pub mod agent_runtime;
pub mod assets;
pub mod channel;
pub mod compaction;
pub mod conversation;
pub mod error;
pub mod exec;
pub mod gameplan;
pub mod harness;
pub mod http;
pub mod jsonschema;
pub mod logging;
pub mod memory;
pub mod orchestrator;
pub mod persona;
pub mod prompting;
pub mod provider;
pub mod pyfmt;
pub mod session;
pub mod sessionfile;
pub mod skill;
pub mod tooling;
pub mod toolkit;
pub mod toolschema;
pub mod utils;
pub mod yaml;

pub use crate::agent::{Agent, Role};
pub use crate::channel::{Channel, ConsoleChannel, FileChannel, LogChannel, MultiChannel};
pub use crate::conversation::{ChatMessage, Conversation, Message, Turn};
pub use crate::error::{Error, Result};
pub use crate::memory::Memory;
pub use crate::persona::PersonaConfig;
pub use crate::provider::{Provider, ProviderResponse};
pub use crate::session::{Session, SessionConfig, SessionResult};
pub use crate::tooling::{ToolCall, ToolHandler, ToolSpec};
pub use crate::toolkit::{ToolDispatcher, ToolResult};
pub use crate::toolschema::ToolDialect;

/// The release this crate implements, matching the Python package's
/// `__version__`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
