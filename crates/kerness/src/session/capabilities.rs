//! Per-invocation identity and scoped capabilities for custom tools.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{lock, store_for, ApprovalMode, RunControl, Shared};
use crate::access::{AccessManager, AccessRequest};
use crate::error::{Error, Result};
use crate::exec;
use crate::tooling::Arguments;

/// Identity assigned by the engine, never read from model arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub(crate) actor: String,
    pub(crate) run_id: String,
    pub(crate) turn_id: u64,
    pub(crate) call_id: String,
}

impl ToolIdentity {
    pub fn actor(&self) -> &str {
        &self.actor
    }
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn turn_id(&self) -> u64 {
        self.turn_id
    }
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
}

/// An action declared before invoking a synchronous custom handler.
/// Preflight must be free of side effects; the handler runs only after approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreflightAction {
    /// Ask the host to confirm this tool invocation.
    Confirm { description: String },
    /// Check a command and its working directory before asking for approval.
    Command {
        command: String,
        cwd: Option<PathBuf>,
    },
}

/// A custom tool with capabilities supplied by the Rust engine.
pub trait ContextToolHandler: Send + Sync {
    /// Declare at most one approval action without performing it. This method
    /// receives identity only; it cannot use the invocation's capabilities.
    fn preflight(
        &self,
        _arguments: &Arguments,
        _identity: &ToolIdentity,
    ) -> Result<Option<PreflightAction>> {
        Ok(None)
    }

    fn call(&self, arguments: &Arguments, context: &ToolContext) -> Result<String>;
}

impl<F> ContextToolHandler for F
where
    F: Fn(&Arguments, &ToolContext) -> Result<String> + Send + Sync,
{
    fn call(&self, arguments: &Arguments, context: &ToolContext) -> Result<String> {
        self(arguments, context)
    }
}

/// A specification for a tool invoked with a scoped context.
#[derive(Clone)]
pub struct ContextToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub handler: Arc<dyn ContextToolHandler>,
}

impl ContextToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: Arc<dyn ContextToolHandler>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            handler,
        }
    }
}

/// Capabilities bound to one invocation. Cloning a handle keeps its identity;
/// it does not extend its lifetime beyond that invocation or widen its access.
#[derive(Clone)]
pub struct ToolContext {
    identity: ToolIdentity,
    shared: Arc<Shared>,
    control: RunControl,
    live: Arc<AtomicBool>,
    approvals: ApprovalMode,
    command_grant: Option<(String, PathBuf, Arc<AtomicBool>)>,
}

pub(super) struct InvocationLease(ToolContext);
impl Drop for InvocationLease {
    fn drop(&mut self) {
        self.0.live.store(false, Ordering::Release);
    }
}

impl ToolContext {
    pub(super) fn new(
        identity: ToolIdentity,
        shared: Arc<Shared>,
        control: RunControl,
        approvals: ApprovalMode,
        grant: Option<&PreflightAction>,
    ) -> Self {
        let command_grant = match grant {
            Some(PreflightAction::Command {
                command,
                cwd: Some(cwd),
            }) => Some((
                command.clone(),
                cwd.clone(),
                Arc::new(AtomicBool::new(true)),
            )),
            _ => None,
        };
        Self {
            identity,
            shared,
            control,
            live: Arc::new(AtomicBool::new(true)),
            approvals,
            command_grant,
        }
    }

    pub fn identity(&self) -> &ToolIdentity {
        &self.identity
    }
    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }

    fn check_live(&self) -> Result<()> {
        if !self.live.load(Ordering::Acquire) {
            return Err(Error::session(
                "Tool capabilities expired when the invocation ended.",
            ));
        }
        if self.is_cancelled() {
            return Err(Error::session("Run cancelled."));
        }
        Ok(())
    }

    pub(super) fn lease(&self) -> InvocationLease {
        InvocationLease(self.clone())
    }

    fn manager(&self) -> AccessManager {
        lock(&self.shared.access).clone()
    }

    pub fn read_file(&self, path: &str) -> Result<String> {
        self.check_live()?;
        exec::read_file(&self.manager(), path, self.identity.actor())
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        self.check_live()?;
        exec::list_dir(&self.manager(), path, self.identity.actor())
    }

    pub fn read_memory(&self) -> Result<String> {
        self.check_live()?;
        let (store, scope) = store_for(&self.shared.memories, self.identity.actor());
        store.read(&scope)
    }

    pub fn write_memory(&self, note: &str) -> Result<bool> {
        self.check_live()?;
        if !self.shared.memory_write {
            return Err(Error::AccessDenied(
                "Memory is read-only for this run.".into(),
            ));
        }
        self.shared.remember(self.identity.actor(), note)
    }

    /// Run a command under this actor's policy. An external approval grants
    /// only its exact command and resolved directory, once. Other unlisted
    /// commands require their own declared preflight action.
    pub fn run_command(
        &self,
        command: &str,
        cwd: Option<&Path>,
        timeout: Option<Duration>,
    ) -> Result<String> {
        self.check_live()?;
        let mut manager = self.manager();
        let cwd = cwd.unwrap_or_else(|| manager.workspace_for(self.identity.actor()));
        let cwd = manager.check_path(
            "Command working directory",
            &cwd.display().to_string(),
            self.identity.actor(),
        )?;
        if self.approvals == ApprovalMode::External {
            manager = manager.with_approval_prompt(None);
        }
        if let Some((allowed, directory, unused)) = &self.command_grant {
            if allowed == command && directory == &cwd {
                let allowed = allowed.trim().to_string();
                let actor = self.identity.actor.clone();
                let unused = Arc::clone(unused);
                manager =
                    manager.with_approval_prompt(Some(Arc::new(move |request: &AccessRequest| {
                        request.kind == "command"
                            && request.target == allowed
                            && request.actor == actor
                            && unused.swap(false, Ordering::AcqRel)
                    })));
            }
        }
        let outcome = exec::run_command_cancellable(
            &manager,
            command,
            Some(&cwd),
            timeout,
            self.identity.actor(),
            &|| self.is_cancelled(),
        );
        super::log_command(
            self.shared.channel.as_ref(),
            command,
            self.identity.actor(),
            &outcome,
        )?;
        outcome
    }
}
