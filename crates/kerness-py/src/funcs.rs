//! The framework's free functions, one Python function each.
//!
//! Nothing is decided here. Every function converts its arguments, calls the
//! `kerness` function of the same name, and converts the answer back. They are
//! collected in one module rather than scattered because the split that matters
//! to a caller is the Python one — `kerness.utils`, `kerness.toolschema` — and
//! that split is made by the shim packages that re-export from here.

use std::cell::RefCell;
use std::path::PathBuf;

use kerness::agent_runtime;
use kerness::compaction;
use kerness::conversation::ChatMessage;
use kerness::gameplan;
use kerness::harness;
use kerness::jsonschema;
use kerness::persona;
use kerness::sessionfile::{self, SessionSnapshot};
use kerness::skill::loader as skill_loader;
use kerness::skill::runtime as skill_runtime;
use kerness::tooling;
use kerness::toolkit;
use kerness::toolschema;
use kerness::utils;
use kerness::{orchestrator, prompting};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use crate::convert::{map_from_py, map_to_py, value_from_py, value_to_py};
use crate::errors::Raise;
use crate::types::{
    dialect_from_py, PyGameplanConfig, PyHarnessSpec, PyPersonaConfig, PyProviderResponse,
    PyResultField, PySkillConfig, PyToolCall, PyToolResult, PyToolSpec, PyTurn,
};

// ---------------------------------------------------------------------- utils

/// Split memory markers out of *text*, returning the text without them.
#[pyfunction]
pub fn parse_memory_markers(text: &str) -> (String, Vec<String>) {
    utils::parse_memory_markers(text)
}

/// Whether *keyword* appears in *text* as a whole word.
#[pyfunction]
pub fn keyword_in_text(text: &str, keyword: &str) -> bool {
    utils::keyword_in_text(text, keyword)
}

/// The participant an orchestrator reply addressed, and what it asked for.
#[pyfunction]
pub fn parse_orchestrator_call(text: &str, agent_names: Vec<String>) -> Option<(String, String)> {
    utils::parse_orchestrator_call(text, &agent_names)
}

/// The terminator an orchestrator reply carried, if any.
#[pyfunction]
#[pyo3(signature = (text, keywords=None))]
pub fn parse_session_end(text: &str, keywords: Option<Vec<String>>) -> Option<String> {
    let keywords = keywords.unwrap_or_else(|| {
        utils::DEFAULT_TERMINATORS
            .iter()
            .map(|word| (*word).to_string())
            .collect()
    });
    utils::parse_session_end(text, &keywords)
}

/// Retry a zero-argument callable, sleeping between attempts.
#[pyfunction]
#[pyo3(signature = (fn_, retries=2, backoff_sec=2.0, interval_sec=None))]
pub fn retry(
    py: Python<'_>,
    fn_: &Bound<'_, PyAny>,
    retries: u32,
    backoff_sec: f64,
    interval_sec: Option<f64>,
) -> PyResult<Py<PyAny>> {
    // The sleeps happen in Rust, so the GIL is dropped for them: a caller
    // retrying a slow network call has other threads to run meanwhile.
    let callable = fn_.clone().unbind();
    py.allow_threads(|| {
        utils::retry(
            || Python::with_gil(|py| callable.bind(py).call0().map(Bound::unbind)),
            retries,
            backoff_sec,
            interval_sec,
        )
    })
}

// -------------------------------------------------------------------- tooling

/// Read `<tool_call>` blocks out of a model's text reply.
#[pyfunction]
pub fn parse_tool_calls(text: &str) -> Vec<PyToolCall> {
    tooling::parse_tool_calls(text)
        .into_iter()
        .map(|inner| PyToolCall { inner })
        .collect()
}

/// Render tool specs as the prompt section a text-dialect model reads.
#[pyfunction]
pub fn format_tools_prompt(tools: Vec<PyToolSpec>) -> String {
    tooling::format_tools_prompt(&specs(&tools))
}

fn specs(tools: &[PyToolSpec]) -> Vec<kerness::tooling::ToolSpec> {
    tools.iter().map(|tool| tool.inner.clone()).collect()
}

// ----------------------------------------------------------------- toolschema

/// One spec as an OpenAI function-tool schema.
#[pyfunction]
pub fn to_openai_tool<'py>(
    py: Python<'py>,
    spec: PyRef<'_, PyToolSpec>,
) -> PyResult<Bound<'py, PyAny>> {
    value_to_py(py, &toolschema::to_openai_tool(&spec.inner))
}

/// One spec as an Anthropic tool schema.
#[pyfunction]
pub fn to_anthropic_tool<'py>(
    py: Python<'py>,
    spec: PyRef<'_, PyToolSpec>,
) -> PyResult<Bound<'py, PyAny>> {
    value_to_py(py, &toolschema::to_anthropic_tool(&spec.inner))
}

/// The schemas to send for *dialect*, or `None` when there is nothing to send.
#[pyfunction]
pub fn tool_schemas<'py>(
    py: Python<'py>,
    dialect: &Bound<'_, PyAny>,
    tools: Vec<PyToolSpec>,
) -> PyResult<Bound<'py, PyAny>> {
    let Some(schemas) = toolschema::tool_schemas(dialect_from_py(dialect)?, &specs(&tools)) else {
        return Ok(py.None().into_bound(py));
    };
    let list = PyList::empty(py);
    for schema in &schemas {
        list.append(value_to_py(py, schema)?)?;
    }
    Ok(list.into_any())
}

/// The tool calls an OpenAI-shaped assistant message carries.
#[pyfunction]
pub fn parse_openai_tool_calls(message: &Bound<'_, PyAny>) -> PyResult<Vec<PyToolCall>> {
    Ok(toolschema::parse_openai_tool_calls(&value_from_py(message)?)
        .into_iter()
        .map(|inner| PyToolCall { inner })
        .collect())
}

/// The tool calls an Anthropic-shaped response carries.
#[pyfunction]
pub fn parse_anthropic_tool_calls(response: &Bound<'_, PyAny>) -> PyResult<Vec<PyToolCall>> {
    Ok(
        toolschema::parse_anthropic_tool_calls(&value_from_py(response)?)
            .into_iter()
            .map(|inner| PyToolCall { inner })
            .collect(),
    )
}

/// The assistant message a reply is appended to the history as.
#[pyfunction]
pub fn render_assistant_turn<'py>(
    py: Python<'py>,
    dialect: &Bound<'_, PyAny>,
    response: PyRef<'_, PyProviderResponse>,
) -> PyResult<Bound<'py, PyAny>> {
    let rendered = toolschema::render_assistant_turn(dialect_from_py(dialect)?, &response.inner);
    value_to_py(py, &rendered)
}

/// The message a tool result is handed back as.
#[pyfunction]
pub fn render_tool_result<'py>(
    py: Python<'py>,
    dialect: &Bound<'_, PyAny>,
    call: PyRef<'_, PyToolCall>,
    result: PyRef<'_, PyToolResult>,
) -> PyResult<Bound<'py, PyAny>> {
    let rendered = toolschema::render_tool_result(
        dialect_from_py(dialect)?,
        &call.inner,
        &result.snapshot(),
    );
    value_to_py(py, &rendered)
}

// ----------------------------------------------------------------- jsonschema

/// Every way *arguments* fails *schema*, or an empty list when it holds.
#[pyfunction]
pub fn validate_arguments(
    schema: &Bound<'_, PyAny>,
    arguments: &Bound<'_, PyDict>,
) -> PyResult<Vec<String>> {
    Ok(jsonschema::validate_arguments(
        &value_from_py(schema)?,
        &map_from_py(arguments)?,
    ))
}

/// Rewrite *json_schema* to satisfy strict mode, and return it.
#[pyfunction]
pub fn ensure_strict<'py>(
    py: Python<'py>,
    json_schema: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut schema = value_from_py(json_schema)?;
    jsonschema::ensure_strict(&mut schema).raise()?;
    value_to_py(py, &schema)
}

// ----------------------------------------------------------------- compaction

/// Estimate the tokens *text* costs.
#[pyfunction]
pub fn estimate_tokens(text: &str) -> usize {
    compaction::estimate_tokens(text)
}

/// Estimate the tokens a rendered turn list costs.
#[pyfunction]
pub fn estimate_turns(turns: Vec<PyTurn>) -> usize {
    compaction::estimate_turns(&raw_turns(&turns))
}

fn raw_turns(turns: &[PyTurn]) -> Vec<kerness::Turn> {
    turns.iter().map(|turn| turn.inner.clone()).collect()
}

/// Shrink *turns* under *limit*, or `None` to leave the conversation alone.
#[pyfunction]
#[pyo3(signature = (turns, *, limit, summarize))]
pub fn compact(
    turns: Vec<PyTurn>,
    limit: usize,
    summarize: &Bound<'_, PyAny>,
) -> PyResult<Option<Vec<PyTurn>>> {
    // A raising summarizer is the caller's error, not an empty summary, so the
    // failure is parked here and re-raised once `compact` has unwound.
    let failure: RefCell<Option<PyErr>> = RefCell::new(None);
    let compacted = compaction::compact(&raw_turns(&turns), limit, |batch| {
        let wrapped: Vec<PyTurn> = batch
            .iter()
            .map(|turn| PyTurn {
                inner: turn.clone(),
            })
            .collect();
        match summarize
            .call1((wrapped,))
            .and_then(|text| text.extract::<String>())
        {
            Ok(text) => text,
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                String::new()
            }
        }
    });
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    Ok(compacted.map(|turns| turns.into_iter().map(|inner| PyTurn { inner }).collect()))
}

/// The messages a summarizer is asked with.
#[pyfunction]
pub fn summary_request<'py>(py: Python<'py>, turns: Vec<PyTurn>) -> PyResult<Bound<'py, PyList>> {
    messages(py, &compaction::summary_request(&raw_turns(&turns)))
}

fn messages<'py>(py: Python<'py>, chat: &[ChatMessage]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for message in chat {
        let dict = PyDict::new(py);
        dict.set_item("role", &message.role)?;
        dict.set_item("content", &message.content)?;
        list.append(dict)?;
    }
    Ok(list)
}

// -------------------------------------------------------------------- harness

/// Read a harness contract out of a gameplan's frontmatter.
#[pyfunction]
#[pyo3(signature = (data, *, source))]
pub fn parse_harness(data: &Bound<'_, PyAny>, source: &str) -> PyResult<PyHarnessSpec> {
    Ok(PyHarnessSpec {
        inner: harness::parse_harness(&value_from_py(data)?, source).raise()?,
    })
}

/// Check a configured session against its contract, returning the tool names it
/// permits.
#[pyfunction]
#[pyo3(signature = (spec, *, participants, orchestrator, registered_tools))]
pub fn validate_harness(
    spec: PyRef<'_, PyHarnessSpec>,
    participants: Vec<String>,
    orchestrator: Option<String>,
    registered_tools: Vec<String>,
) -> PyResult<Vec<String>> {
    harness::validate_harness(
        &spec.inner,
        &participants,
        orchestrator.as_deref(),
        &registered_tools,
    )
    .raise()
}

// ------------------------------------------------------------------- gameplan

/// Load a gameplan by builtin name or path.
#[pyfunction]
pub fn load_gameplan(name_or_path: &str) -> PyResult<PyGameplanConfig> {
    Ok(PyGameplanConfig {
        inner: gameplan::load_gameplan(name_or_path).raise()?,
    })
}

/// The gameplans that ship with the package.
#[pyfunction]
pub fn list_builtin_gameplans() -> Vec<String> {
    gameplan::list_builtin_gameplans()
}

// -------------------------------------------------------------------- persona

/// Load a persona by builtin name or path.
#[pyfunction]
#[pyo3(signature = (path, *, search=None))]
pub fn load_persona(path: &str, search: Option<Vec<PathBuf>>) -> PyResult<PyPersonaConfig> {
    Ok(PyPersonaConfig::adopt(
        persona::load_persona(path, &search.unwrap_or_default()).raise()?,
    ))
}

/// Render a persona as the prompt section an agent is given.
#[pyfunction]
pub fn format_persona_for_prompt(config: PyRef<'_, PyPersonaConfig>) -> String {
    persona::format_persona_for_prompt(&config.snapshot())
}

/// The personas that ship with the package.
#[pyfunction]
pub fn list_builtin_personas() -> Vec<String> {
    persona::list_builtin_personas()
}

/// The file a persona name or path resolves to.
#[pyfunction]
#[pyo3(signature = (path, *, search=None))]
pub fn resolve_persona_path(
    py: Python<'_>,
    path: &str,
    search: Option<Vec<PathBuf>>,
) -> PyResult<Py<PyAny>> {
    let resolved = persona::resolve_persona_path(path, &search.unwrap_or_default()).raise()?;
    crate::types::path_to_py(py, resolved.to_string_lossy().as_ref())
}

// -------------------------------------------------------------------- toolkit

/// Narrow *tools* to *allowed*, in registration order.
#[pyfunction]
#[pyo3(signature = (tools, allowed=None))]
pub fn resolve(tools: Vec<PyToolSpec>, allowed: Option<Vec<String>>) -> Vec<PyToolSpec> {
    let kept = toolkit::resolve(&specs(&tools), allowed.as_deref());
    tools
        .into_iter()
        .filter(|tool| kept.iter().any(|spec| spec.name == tool.inner.name))
        .collect()
}

// ---------------------------------------------------------------------- skill

/// Load a skill by builtin name or path.
#[pyfunction]
pub fn load_skill(name_or_path: &str) -> PyResult<PySkillConfig> {
    Ok(PySkillConfig {
        inner: skill_loader::load_skill(name_or_path).raise()?,
    })
}

/// The skills that ship with the package.
#[pyfunction]
pub fn list_builtin_skills() -> Vec<String> {
    skill_loader::list_builtin_skills()
}

/// Render the one-line-per-skill index an agent sees in its prompt.
#[pyfunction]
pub fn format_skills_index(skills: Vec<PySkillConfig>) -> String {
    let configs: Vec<_> = skills.iter().map(|skill| skill.inner.clone()).collect();
    skill_runtime::format_skills_index(&configs)
}

/// Narrow a toolkit to what the active skills permit.
#[pyfunction]
#[pyo3(signature = (tools, gate=None))]
pub fn apply_gate(
    tools: Vec<PyToolSpec>,
    gate: Option<&Bound<'_, PyAny>>,
) -> PyResult<Vec<PyToolSpec>> {
    // A set is what callers hold — the gate is a membership test, and the
    // skills that contribute to it overlap — so anything iterable of names is
    // taken rather than a list alone.
    let gate = match gate.filter(|object| !object.is_none()) {
        None => None,
        Some(names) => Some(
            names
                .try_iter()?
                .map(|name| name?.extract::<String>())
                .collect::<PyResult<_>>()?,
        ),
    };
    let kept = skill_runtime::apply_gate(&specs(&tools), gate.as_ref());
    Ok(tools
        .into_iter()
        .filter(|tool| kept.iter().any(|spec| spec.name == tool.inner.name))
        .collect())
}

// ------------------------------------------------------------------ prompting

/// Render the memory section of a system prompt.
#[pyfunction]
#[pyo3(signature = (memory, *, writable=false))]
pub fn memory_block(memory: &Bound<'_, PyAny>, writable: bool) -> PyResult<String> {
    let content: String = memory.call_method0("read")?.extract()?;
    Ok(prompting::memory_block(&content, writable))
}

// ----------------------------------------------------------------- closing turn

/// The prompt the closing turn asks its result shape with.
#[pyfunction]
pub fn closing_prompt(fields: Vec<PyResultField>) -> String {
    orchestrator::closing_prompt(&result_fields(&fields))
}

fn result_fields(fields: &[PyResultField]) -> Vec<kerness::harness::ResultField> {
    fields.iter().map(PyResultField::snapshot).collect()
}

/// The prompt that hands a closing draft back for revision.
#[pyfunction]
pub fn verdict_rethink_prompt(draft: &str, fields: Vec<PyResultField>) -> String {
    orchestrator::verdict_rethink_prompt(draft, &result_fields(&fields))
}

/// Read the declared result fields out of a closing reply.
#[pyfunction]
pub fn parse_result_fields<'py>(
    py: Python<'py>,
    text: &str,
    fields: Vec<PyResultField>,
) -> PyResult<Bound<'py, PyDict>> {
    map_to_py(
        py,
        &orchestrator::parse_result_fields(text, &result_fields(&fields)),
    )
}

/// Strip the closing reply's result block, leaving the prose.
#[pyfunction]
pub fn strip_result_block(text: &str) -> String {
    orchestrator::strip_result_block(text)
}

// ---------------------------------------------------------------- session file

/// Describe the run a snapshot belongs to.
#[pyfunction]
#[pyo3(signature = (*, gameplan, topic, participants, orchestrator))]
pub fn identity_for<'py>(
    py: Python<'py>,
    gameplan: &str,
    topic: &str,
    participants: Vec<String>,
    orchestrator: &str,
) -> PyResult<Bound<'py, PyDict>> {
    map_to_py(
        py,
        &sessionfile::identity_for(gameplan, topic, &participants, orchestrator),
    )
}

/// Fail unless *saved* describes the same run as *current*.
#[pyfunction]
pub fn check_identity(saved: &Bound<'_, PyDict>, current: &Bound<'_, PyDict>) -> PyResult<()> {
    sessionfile::check_identity(&map_from_py(saved)?, &map_from_py(current)?).raise()
}

/// Everything needed to continue a run.
#[pyclass(name = "SessionSnapshot", module = "kerness._core")]
#[derive(Clone)]
pub struct PySessionSnapshot {
    pub inner: SessionSnapshot,
}

#[pymethods]
impl PySessionSnapshot {
    #[new]
    // `loop` is a Rust keyword; the raw identifier keeps the Python keyword.
    #[pyo3(signature = (identity=None, turns=None, transcript=None, r#loop=None, compactions=0))]
    fn new(
        identity: Option<&Bound<'_, PyAny>>,
        turns: Option<Vec<PyTurn>>,
        transcript: Option<Vec<crate::types::PyMessage>>,
        r#loop: Option<&Bound<'_, PyAny>>,
        compactions: i64,
    ) -> PyResult<Self> {
        Ok(PySessionSnapshot {
            inner: SessionSnapshot {
                identity: crate::convert::optional_map(identity)?,
                turns: raw_turns(&turns.unwrap_or_default()),
                transcript: transcript
                    .unwrap_or_default()
                    .into_iter()
                    .map(|message| message.inner)
                    .collect(),
                loop_state: crate::convert::optional_map(r#loop)?,
                compactions,
            },
        })
    }

    #[getter]
    fn identity<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.identity)
    }

    #[getter]
    fn turns(&self) -> Vec<PyTurn> {
        self.inner
            .turns
            .iter()
            .map(|turn| PyTurn {
                inner: turn.clone(),
            })
            .collect()
    }

    #[getter]
    fn transcript(&self) -> Vec<crate::types::PyMessage> {
        self.inner
            .transcript
            .iter()
            .map(|message| crate::types::PyMessage {
                inner: message.clone(),
            })
            .collect()
    }

    // `loop` is a Rust keyword, so the Python name is spelled out separately.
    #[getter]
    #[pyo3(name = "loop")]
    fn loop_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        map_to_py(py, &self.inner.loop_state)
    }

    #[getter]
    fn compactions(&self) -> i64 {
        self.inner.compactions
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<PySessionSnapshot>()
            .is_ok_and(|other| other.inner == self.inner)
    }
}

/// Write a snapshot, atomically.
#[pyfunction]
pub fn save_snapshot(path: PathBuf, snapshot: PyRef<'_, PySessionSnapshot>) -> PyResult<()> {
    sessionfile::save_snapshot(&path, &snapshot.inner).raise()
}

/// Read a snapshot back, or `None` when the file is absent.
#[pyfunction]
pub fn load_snapshot(path: PathBuf) -> PyResult<Option<PySessionSnapshot>> {
    Ok(sessionfile::load_snapshot(&path)
        .raise()?
        .map(|inner| PySessionSnapshot { inner }))
}

/// The chat message a single turn renders to.
#[pyfunction]
pub fn render_turn<'py>(py: Python<'py>, turn: PyRef<'_, PyTurn>) -> PyResult<Bound<'py, PyDict>> {
    let ChatMessage { role, content } = turn.inner.render();
    let dict = PyDict::new(py);
    dict.set_item("role", role)?;
    dict.set_item("content", content)?;
    Ok(dict)
}

/// Register every function in this module on `_core`.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = module.py();
    module.add(
        "DEFAULT_TERMINATORS",
        PyTuple::new(py, utils::DEFAULT_TERMINATORS.iter().copied())?,
    )?;
    module.add("INVALID_CALL", tooling::INVALID_CALL)?;
    module.add("SKILL_TOOL_NAME", skill_runtime::SKILL_TOOL_NAME)?;
    module.add("SCHEMA_VERSION", sessionfile::SCHEMA_VERSION)?;
    module.add("CHARS_PER_TOKEN", compaction::CHARS_PER_TOKEN)?;
    module.add("COMPACT_TO_FRACTION", compaction::COMPACT_TO_FRACTION)?;
    module.add("SUMMARY_PREFIX", compaction::SUMMARY_PREFIX)?;
    module.add("SUMMARY_PROMPT", compaction::SUMMARY_PROMPT)?;
    module.add("MEMORY_HEADER", prompting::MEMORY_HEADER)?;
    module.add("MEMORY_WRITE_HINT", prompting::MEMORY_WRITE_HINT)?;
    module.add("FORCED_END_NOTE", orchestrator::FORCED_END_NOTE)?;
    module.add("FOLLOWUP_PROMPT", agent_runtime::FOLLOWUP_PROMPT)?;
    module.add("MAX_INVALID_CALLS", agent_runtime::MAX_INVALID_CALLS)?;
    module.add(
        "BUNDLE_DIRS",
        PyTuple::new(py, skill_loader::BUNDLE_DIRS.iter().copied())?,
    )?;
    module.add(
        "RESERVED_TOOL_NAMES",
        PyTuple::new(py, harness::RESERVED_TOOL_NAMES.iter().copied())?,
    )?;

    module.add_class::<PySessionSnapshot>()?;

    macro_rules! add {
        ($($name:ident),* $(,)?) => {
            $(module.add_function(wrap_pyfunction!($name, module)?)?;)*
        };
    }
    add!(
        parse_memory_markers,
        keyword_in_text,
        parse_orchestrator_call,
        parse_session_end,
        retry,
        parse_tool_calls,
        format_tools_prompt,
        to_openai_tool,
        to_anthropic_tool,
        tool_schemas,
        parse_openai_tool_calls,
        parse_anthropic_tool_calls,
        render_assistant_turn,
        render_tool_result,
        validate_arguments,
        ensure_strict,
        estimate_tokens,
        estimate_turns,
        compact,
        summary_request,
        parse_harness,
        validate_harness,
        load_gameplan,
        list_builtin_gameplans,
        load_persona,
        format_persona_for_prompt,
        list_builtin_personas,
        resolve_persona_path,
        resolve,
        load_skill,
        list_builtin_skills,
        format_skills_index,
        apply_gate,
        memory_block,
        closing_prompt,
        verdict_rethink_prompt,
        parse_result_fields,
        strip_result_block,
        identity_for,
        check_identity,
        save_snapshot,
        load_snapshot,
        render_turn,
    );
    Ok(())
}
