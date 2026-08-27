//! Skill activation and the registry that hands it out.
//!
//! Both types are stateful across a turn — an activation remembers what it has
//! already loaded and how the loaded skills narrowed the toolkit — so the Rust
//! object is shared rather than copied: [`PySkillActivation`] holds the same
//! `Arc` the registry built, and a caller holding it sees the narrowing a tool
//! call performed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use kerness::skill::loader::SkillConfig;
use kerness::skill::runtime::{GrantPaths, SkillActivation, SkillRegistry, SkillsFor};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PySet};

use crate::errors::Raise;
use crate::types::{path_to_py, PySkillConfig, PyToolSpec};

/// Wrap a Python callable as the grant hook, or `None` to disable the grant.
fn bind_grant(object: Option<&Bound<'_, PyAny>>) -> Option<GrantPaths> {
    let object = object.filter(|object| !object.is_none())?.clone().unbind();
    Some(Arc::new(move |paths: &[PathBuf]| {
        // The grant is an effect, not a decision: a hook that raises has failed
        // to widen access, which leaves the session exactly as restricted as it
        // was. Turning that into a failed run would be worse than the refusal.
        let _ = Python::with_gil(|py| -> PyResult<()> {
            let listed: Vec<Py<PyAny>> = paths
                .iter()
                .map(|path| path_to_py(py, path.to_string_lossy().as_ref()))
                .collect::<PyResult<_>>()?;
            object.bind(py).call1((listed,))?;
            Ok(())
        });
    }))
}

/// One agent's skills for one turn, and what they have narrowed so far.
#[pyclass(name = "SkillActivation", module = "kerness._core")]
pub struct PySkillActivation {
    pub inner: Arc<SkillActivation>,
}

#[pymethods]
impl PySkillActivation {
    #[new]
    #[pyo3(signature = (skills, grant_paths=None))]
    fn new(skills: &Bound<'_, PyDict>, grant_paths: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        let mut configs: BTreeMap<String, SkillConfig> = BTreeMap::new();
        for (name, config) in skills.iter() {
            configs.insert(name.extract()?, config.extract::<PySkillConfig>()?.inner);
        }
        Ok(PySkillActivation {
            inner: Arc::new(SkillActivation::new(configs, bind_grant(grant_paths))),
        })
    }

    /// The skills this agent may load, sorted.
    #[getter]
    fn names(&self) -> Vec<String> {
        self.inner.names()
    }

    /// Tool names the skills loaded so far permit, or `None` for no narrowing.
    #[getter]
    fn gate<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.gate() {
            None => Ok(py.None().into_bound(py)),
            Some(names) => Ok(PySet::new(py, names.iter())?.into_any()),
        }
    }

    /// Activate a skill and return what the agent should read.
    fn load(&self, name: &str) -> PyResult<String> {
        self.inner.load(name).raise()
    }
}

/// Hands out per-agent activations and builds the `Skill` tool.
#[pyclass(name = "SkillRegistry", module = "kerness._core")]
pub struct PySkillRegistry {
    pub inner: SkillRegistry,
}

#[pymethods]
impl PySkillRegistry {
    #[new]
    #[pyo3(signature = (skills_for, grant_paths=None))]
    fn new(skills_for: &Bound<'_, PyAny>, grant_paths: Option<&Bound<'_, PyAny>>) -> Self {
        let lookup = skills_for.clone().unbind();
        // A lookup that raises leaves the agent with no skills, which is the
        // same position an agent that declared none is in — the alternative is
        // failing a turn over a prompt section.
        let skills_for: SkillsFor = Arc::new(move |name: &str| {
            Python::with_gil(|py| {
                lookup
                    .bind(py)
                    .call1((name,))
                    .and_then(|found| found.extract::<Vec<PySkillConfig>>())
                    .map(|found| found.into_iter().map(|skill| skill.inner).collect())
                    .unwrap_or_default()
            })
        });
        PySkillRegistry {
            inner: SkillRegistry::new(skills_for, bind_grant(grant_paths)),
        }
    }

    /// Start a fresh activation for one agent turn.
    fn activation_for(&self, agent_name: &str) -> PySkillActivation {
        PySkillActivation {
            inner: self.inner.activation_for(agent_name),
        }
    }

    /// The `Skill` tool bound to *activation*, or `None` when it has no skills.
    fn build_tool(
        &self,
        py: Python<'_>,
        activation: PyRef<'_, PySkillActivation>,
    ) -> Option<PyToolSpec> {
        self.inner
            .build_tool(&activation.inner)
            .map(|spec| PyToolSpec::adopt(py, spec))
    }
}
