//! Access control, and the policy object callers actually hold.
//!
//! `AccessPolicy` is a Python dataclass rather than a class declared here, and
//! the reason is its list fields: a caller may build one, hand it to a manager,
//! and then `append` to a list on it — and the manager is required *not* to see
//! that, because it snapshots on construction. Python list semantics are
//! exactly what that contract is written in, so the policy stays a Python
//! object and Rust reads a snapshot out of it.
//!
//! [`PyAccessManager`] keeps the caller's policy object alongside the snapshot,
//! which is what lets `allow_dirs` write its grant back to the object a later
//! manager will be rebuilt from.

use std::collections::BTreeMap;
use std::sync::Arc;

use kerness::access::{AccessManager, AccessPolicy, AccessRequest, ApprovePrompt};
use kerness::pyfmt::repr_str;
use pyo3::prelude::*;

use crate::errors::Raise;
use crate::types::{path_to_py, pybool};

// -------------------------------------------------------------- AccessRequest

/// A single access request, as an approver is shown it.
#[pyclass(name = "AccessRequest", module = "kerness._core", frozen, get_all)]
#[derive(Clone)]
pub struct PyAccessRequest {
    /// `"command"` — the only kind the framework raises.
    pub kind: String,
    /// `"run"`.
    pub action: String,
    pub target: String,
    pub actor: String,
}

impl PyAccessRequest {
    pub fn adopt(request: &AccessRequest) -> Self {
        PyAccessRequest {
            kind: request.kind.clone(),
            action: request.action.clone(),
            target: request.target.clone(),
            actor: request.actor.clone(),
        }
    }
}

#[pymethods]
impl PyAccessRequest {
    #[new]
    #[pyo3(signature = (kind, action, target, actor=String::new()))]
    fn new(kind: String, action: String, target: String, actor: String) -> Self {
        PyAccessRequest {
            kind,
            action,
            target,
            actor,
        }
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<PyAccessRequest>().is_ok_and(|other| {
            other.kind == self.kind
                && other.action == self.action
                && other.target == self.target
                && other.actor == self.actor
        })
    }

    fn __hash__(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        (&self.kind, &self.action, &self.target, &self.actor).hash(&mut hasher);
        hasher.finish()
    }

    fn __repr__(&self) -> String {
        format!(
            "AccessRequest(kind={}, action={}, target={}, actor={})",
            repr_str(&self.kind),
            repr_str(&self.action),
            repr_str(&self.target),
            repr_str(&self.actor),
        )
    }
}

// --------------------------------------------------------------- AccessPolicy

/// A Python approver, seen as an [`ApprovePrompt`].
struct PyApprove {
    callable: Py<PyAny>,
}

impl ApprovePrompt for PyApprove {
    fn approve(&self, request: &AccessRequest) -> bool {
        // A raising approver is a denial: the decision is the caller's, and a
        // caller whose approver blew up has not made one.
        Python::with_gil(|py| {
            self.callable
                .bind(py)
                .call1((PyAccessRequest::adopt(request),))
                .and_then(|answer| answer.is_truthy())
                .unwrap_or(false)
        })
    }
}

/// Read a Python `AccessPolicy` into the framework's own.
///
/// A snapshot, deliberately: a manager built from a policy does not track later
/// edits to it, and rebuilding one is how a caller applies them.
pub fn policy_from_py(object: &Bound<'_, PyAny>) -> PyResult<AccessPolicy> {
    let approve_prompt = object.getattr("approve_prompt")?;
    Ok(AccessPolicy {
        approve_prompt: (!approve_prompt.is_none()).then(|| {
            Arc::new(PyApprove {
                callable: approve_prompt.unbind(),
            }) as Arc<dyn ApprovePrompt>
        }),
        auto_approve_prefixes: object.getattr("auto_approve_prefixes")?.extract()?,
        workspace: path_string(&object.getattr("workspace")?)?,
        agent_workspaces: agent_workspaces(&object.getattr("agent_workspaces")?)?,
        allowed_commands: object.getattr("allowed_commands")?.extract()?,
        allowed_command_patterns: object.getattr("allowed_command_patterns")?.extract()?,
        allowed_files: path_strings(&object.getattr("allowed_files")?)?,
        allowed_dirs: path_strings(&object.getattr("allowed_dirs")?)?,
        allowed_hosts: object.getattr("allowed_hosts")?.extract()?,
        trust_skill_bundles: object.getattr("trust_skill_bundles")?.is_truthy()?,
    })
}

/// The text of an optional path, which may be `str` or `Path`.
fn path_string(object: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    if object.is_none() {
        return Ok(None);
    }
    Ok(Some(object.str()?.to_string_lossy().into_owned()))
}

/// The per-agent workspaces, read out of a `dict` of agent name to path.
fn agent_workspaces(object: &Bound<'_, PyAny>) -> PyResult<BTreeMap<String, String>> {
    let mut workspaces = BTreeMap::new();
    for item in object.call_method0("items")?.try_iter()? {
        let (agent, workspace): (String, Bound<'_, PyAny>) = item?.extract()?;
        workspaces.insert(agent, workspace.str()?.to_string_lossy().into_owned());
    }
    Ok(workspaces)
}

/// The text of a path list whose entries may be `str` or `Path`.
fn path_strings(object: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    let mut paths = Vec::new();
    for item in object.try_iter()? {
        paths.push(item?.str()?.to_string_lossy().into_owned());
    }
    Ok(paths)
}

/// Build a fresh Python `AccessPolicy`, for a caller that passed none.
fn default_policy(py: Python<'_>) -> PyResult<Py<PyAny>> {
    Ok(py
        .import("kerness.access")?
        .getattr("AccessPolicy")?
        .call0()?
        .unbind())
}

// -------------------------------------------------------------- AccessManager

/// Evaluates access requests against a policy.
#[pyclass(name = "AccessManager", module = "kerness._core")]
pub struct PyAccessManager {
    inner: AccessManager,
    /// The caller's policy object. Held so `_policy` reads back the object that
    /// was passed — approver identity included — and so `allow_dirs` can record
    /// its grant where a rebuilt manager will find it.
    policy: Py<PyAny>,
}

impl PyAccessManager {
    /// The framework manager, for a session assembling itself around one.
    pub fn snapshot(&self) -> PyResult<AccessManager> {
        Python::with_gil(|py| Ok(AccessManager::new(policy_from_py(self.policy.bind(py))?)))
    }
}

#[pymethods]
impl PyAccessManager {
    #[new]
    #[pyo3(signature = (policy=None))]
    fn new(py: Python<'_>, policy: Option<Bound<'_, PyAny>>) -> PyResult<Self> {
        let policy = match policy.filter(|object| !object.is_none()) {
            Some(object) => object.unbind(),
            None => default_policy(py)?,
        };
        Ok(PyAccessManager {
            inner: AccessManager::new(policy_from_py(policy.bind(py))?),
            policy,
        })
    }

    /// The policy this manager was built from, including any mid-session grant.
    #[getter]
    fn _policy(&self, py: Python<'_>) -> Py<PyAny> {
        self.policy.clone_ref(py)
    }

    /// Validate a command execution request.
    #[pyo3(signature = (command, actor=""))]
    fn check_command(&self, command: &str, actor: &str) -> PyResult<()> {
        self.inner.check_command(command, actor).raise()
    }

    /// Validate a network destination against ``allowed_hosts``.
    ///
    /// *target* may be a URL or a bare hostname. An empty ``allowed_hosts``
    /// allows everything.
    #[pyo3(signature = (target, actor=""))]
    fn check_host(&self, target: &str, actor: &str) -> PyResult<()> {
        self.inner.check_host(target, actor).raise()
    }

    /// Validate a file or directory access request, returning the resolved path.
    #[pyo3(signature = (action, path, actor=""))]
    fn check_path(
        &self,
        py: Python<'_>,
        action: &str,
        path: &Bound<'_, PyAny>,
        actor: &str,
    ) -> PyResult<Py<PyAny>> {
        let path = path.str()?.to_string_lossy().into_owned();
        let resolved = self.inner.check_path(action, &path, actor).raise()?;
        path_to_py(py, resolved.to_string_lossy().as_ref())
    }

    /// Grant read access to directories mid-session.
    ///
    /// The caller's policy is updated too, so a manager rebuilt from it later
    /// keeps the grant.
    fn allow_dirs(&mut self, py: Python<'_>, paths: &Bound<'_, PyAny>) -> PyResult<()> {
        let granted = path_strings(paths)?;
        let allowed = self.policy.bind(py).getattr("allowed_dirs")?;
        for path in &granted {
            allowed.call_method1("append", (path.as_str(),))?;
        }
        self.inner.allow_dirs(granted);
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "AccessManager(trust_skill_bundles={})",
            pybool(self.inner.policy().trust_skill_bundles),
        )
    }
}
