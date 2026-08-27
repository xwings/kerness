//! The harness contract.
//!
//! A harness is the machine-readable definition of how a session runs: which
//! roles must exist, how the loop advances and terminates, which tools agents
//! may call, which skills they may load, and what shape the result takes.
//!
//! The harness is declared in a gameplan's YAML frontmatter. Nothing about the
//! flow is hardcoded in the runtime — this module parses the contract,
//! validates it, and hands the runtime a [`HarnessSpec`] to configure itself
//! from.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::pyfmt;

/// Tool names the runtime owns.
///
/// A gameplan cannot declare a tool with these names, because the runtime
/// builds them per agent.
pub const RESERVED_TOOL_NAMES: &[&str] = &["Skill"];

static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[-_][a-z0-9]+)*$").expect("static pattern"));

/// The types a result field may declare, and the spellings each accepts.
const RESULT_TYPES: &[(&str, ResultType)] = &[
    ("bool", ResultType::Bool),
    ("boolean", ResultType::Bool),
    ("dict", ResultType::Dict),
    ("float", ResultType::Float),
    ("int", ResultType::Int),
    ("integer", ResultType::Int),
    ("list", ResultType::List),
    ("number", ResultType::Float),
    ("str", ResultType::Str),
    ("string", ResultType::Str),
];

/// What a result field coerces to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultType {
    Str,
    Int,
    Float,
    Bool,
    List,
    Dict,
}

/// Whether the harness needs an orchestrator, and what it is called.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrchestratorSpec {
    pub required: bool,
    /// Optional prompt appended to the built-in orchestrator instructions.
    pub instruction: String,
}

/// How many participants the harness accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantSpec {
    pub min: i64,
    pub max: Option<i64>,
}

impl Default for ParticipantSpec {
    fn default() -> Self {
        ParticipantSpec { min: 1, max: None }
    }
}

/// The role contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentsSpec {
    pub orchestrator: OrchestratorSpec,
    pub participants: ParticipantSpec,
}

/// One stage of the session, with the instruction that defines it.
///
/// Phases are how a gameplan expresses a *think → rethink* structure: an early
/// phase asks each agent for an independent position, a later phase asks them
/// to revisit that position now that they have seen the others. The
/// distinction matters because an agent that never revisits its opening move
/// produces a transcript of parallel monologues rather than a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseSpec {
    /// Slug identifying the phase (e.g. `think`, `rethink`).
    pub name: String,
    /// Text handed to the orchestrator while the phase is active, and passed
    /// on to participants as their turn instruction.
    pub instruction: String,
    /// How many full rounds the phase lasts. A round completes when every
    /// participant has taken one turn.
    pub rounds: i64,
    /// Marks the phase as a revision phase. Participants in a rethink phase
    /// are explicitly told to re-examine their own earlier position and to say
    /// plainly whether it changed.
    pub rethink: bool,
}

impl Default for PhaseSpec {
    fn default() -> Self {
        PhaseSpec {
            name: String::new(),
            instruction: String::new(),
            rounds: 1,
            rethink: false,
        }
    }
}

/// How the session advances and how it stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopSpec {
    pub max_turns: i64,
    pub max_rounds: i64,
    pub terminate_on: Vec<String>,
    pub phases: Vec<PhaseSpec>,
    /// Keyword the orchestrator uses to advance a phase early.
    pub advance_on: String,
    /// How many times to re-ask an orchestrator whose reply named no
    /// participant and contained no terminator, before forcing the end.
    pub orchestrator_retries: i64,
    /// Whether the closing verdict gets a second pass. On by default: the
    /// orchestrator is also the judge, and a judge that writes its verdict in
    /// one shot has never read it back against the transcript. Participants
    /// get their rethink from the phase list; this is the judge's. Costs one
    /// extra provider call per session, which is why it can be turned off.
    pub verdict_rethink: bool,
}

impl Default for LoopSpec {
    fn default() -> Self {
        LoopSpec {
            max_turns: 50,
            max_rounds: 3,
            terminate_on: vec!["END_SESSION".to_string()],
            phases: Vec::new(),
            advance_on: "NEXT_PHASE".to_string(),
            orchestrator_retries: 2,
            verdict_rethink: true,
        }
    }
}

impl LoopSpec {
    /// The terminator that means agreement, if the harness has one.
    pub fn consensus_keyword(&self) -> Option<&str> {
        self.terminate_on
            .iter()
            .find(|keyword| keyword.to_uppercase().contains("CONSENSUS"))
            .map(String::as_str)
    }
}

/// One field the orchestrator must report when the session ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultField {
    pub name: String,
    pub type_name: String,
    pub description: String,
}

impl Default for ResultField {
    fn default() -> Self {
        ResultField {
            name: String::new(),
            type_name: "str".to_string(),
            description: String::new(),
        }
    }
}

impl ResultField {
    /// The type this field coerces to.
    ///
    /// Unknown spellings are refused at load, so the fallback only covers a
    /// field built directly rather than parsed — and `str` is what an
    /// unannotated field would have been anyway.
    pub fn result_type(&self) -> ResultType {
        result_type(&self.type_name).unwrap_or(ResultType::Str)
    }
}

fn result_type(name: &str) -> Option<ResultType> {
    RESULT_TYPES
        .iter()
        .find(|(spelling, _)| *spelling == name)
        .map(|(_, kind)| *kind)
}

/// The complete parsed harness contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessSpec {
    pub name: String,
    pub description: String,
    pub agents: AgentsSpec,
    pub loop_spec: LoopSpec,
    /// Tool names this harness permits. `None` means "every registered tool";
    /// a list narrows. See the narrow/widen rule in [`HarnessSpec::resolve_tools`].
    pub tools: Option<Vec<String>>,
    /// Skill names this harness adds. `None` means "session skills only".
    pub skills: Option<Vec<String>>,
    pub result: Vec<ResultField>,
}

impl HarnessSpec {
    /// The tool names available under this harness.
    ///
    /// **Tools narrow.** A gameplan can only restrict the set, never extend
    /// it, because a tool is a handler supplied by the host program — a
    /// gameplan naming a tool nobody registered is naming nothing. That is an
    /// error rather than a silent drop, because silently ignoring a declared
    /// tool is how a session runs to completion doing none of what the
    /// gameplan asked for.
    ///
    /// Returns the permitted tool names in *registration* order.
    pub fn resolve_tools(&self, registered: &[String]) -> Result<Vec<String>> {
        let Some(declared) = &self.tools else {
            return Ok(registered.to_vec());
        };
        let unknown: BTreeSet<&str> = declared
            .iter()
            .filter(|name| !registered.contains(name))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let listed = unknown.into_iter().collect::<Vec<_>>().join(", ");
            let joined = registered.join(", ");
            let registered_list = if joined.is_empty() { "(none)" } else { &joined };
            return Err(Error::Session(format!(
                "Gameplan '{}' requires tool(s) {listed} which are not registered. \
                 Register them with session.add_tool(...) before run(). \
                 Registered: {registered_list}",
                self.name
            )));
        }
        Ok(registered
            .iter()
            .filter(|name| declared.contains(name))
            .cloned()
            .collect())
    }

    /// The skill names available under this harness.
    ///
    /// **Skills widen.** A skill is inert instruction text with no handler
    /// behind it, so a gameplan declaring one can load it itself. The result
    /// is the union of session-level and harness-declared skills, order
    /// preserved and duplicates dropped.
    pub fn resolve_skills(&self, session_skills: &[String]) -> Vec<String> {
        let mut combined: Vec<String> = Vec::with_capacity(session_skills.len());
        for name in session_skills.iter().chain(self.skills.iter().flatten()) {
            if !combined.contains(name) {
                combined.push(name.clone());
            }
        }
        combined
    }
}

/// Build a [`HarnessSpec`] from parsed frontmatter.
///
/// Every validation here is a *load-time* one: it depends only on the file,
/// not on how a session was configured. Run-time checks live in
/// [`validate_harness`].
pub fn parse_harness(data: &Value, source: &str) -> Result<HarnessSpec> {
    let Value::Object(data) = data else {
        return Err(Error::GameplanLoad(format!(
            "Gameplan frontmatter in {source} must be a mapping, got {}.",
            type_name(data)
        )));
    };

    let name = text(data, "name");
    if !name.is_empty() && !SLUG_RE.is_match(&name) {
        return Err(Error::GameplanLoad(format!(
            "Gameplan name '{name}' in {source} must be a lowercase slug \
             (letters, digits, '-' or '_')."
        )));
    }

    Ok(HarnessSpec {
        name,
        description: text(data, "description"),
        agents: parse_agents(get(data, "agents"), source)?,
        loop_spec: parse_loop(get(data, "loop"), source)?,
        tools: parse_name_list(get(data, "tools"), "tools", source)?,
        skills: parse_name_list(get(data, "skills"), "skills", source)?,
        result: parse_result(get(data, "result"), source)?,
    })
}

/// Check a configured session against the contract.
///
/// Returns the tool names permitted for this session, and reports every
/// problem the caller can fix at once rather than one per run.
pub fn validate_harness(
    spec: &HarnessSpec,
    participants: &[String],
    orchestrator: Option<&str>,
    registered_tools: &[String],
) -> Result<Vec<String>> {
    let mut problems: Vec<String> = Vec::new();

    if spec.agents.orchestrator.required && orchestrator.is_none_or(|name| name.is_empty()) {
        problems.push(format!(
            "gameplan '{}' requires an orchestrator; \
             add one with session.add_orchestrator(...)",
            spec.name
        ));
    }

    let count = participants.len() as i64;
    let bounds = &spec.agents.participants;
    if count < bounds.min {
        problems.push(format!(
            "gameplan '{}' requires at least {} participant(s), got {count}",
            spec.name, bounds.min
        ));
    }
    if let Some(maximum) = bounds.max {
        if count > maximum {
            problems.push(format!(
                "gameplan '{}' allows at most {maximum} participant(s), got {count}: {}",
                spec.name,
                participants.join(", ")
            ));
        }
    }

    let duplicates: BTreeSet<&str> = participants
        .iter()
        .filter(|name| participants.iter().filter(|other| other == name).count() > 1)
        .map(String::as_str)
        .collect();
    if !duplicates.is_empty() {
        problems.push(format!(
            "duplicate agent name(s): {} — agents are addressed by name, so names \
             must be unique",
            duplicates.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(orchestrator) = orchestrator.filter(|name| !name.is_empty()) {
        if participants.iter().any(|name| name == orchestrator) {
            problems.push(format!(
                "orchestrator '{orchestrator}' shares a name with a participant"
            ));
        }
    }

    let mut tools = Vec::new();
    match spec.resolve_tools(registered_tools) {
        Ok(resolved) => tools = resolved,
        Err(error) => problems.push(error.to_string()),
    }

    if !problems.is_empty() {
        let listed: Vec<String> = problems
            .iter()
            .map(|problem| format!("  - {problem}"))
            .collect();
        return Err(Error::Session(format!(
            "Session does not satisfy the harness:\n{}",
            listed.join("\n")
        )));
    }
    Ok(tools)
}

fn parse_agents(raw: Option<&Value>, source: &str) -> Result<AgentsSpec> {
    let Some(raw) = raw else {
        return Ok(AgentsSpec::default());
    };
    let Value::Object(raw) = raw else {
        return Err(Error::GameplanLoad(format!(
            "'agents' in {source} must be a mapping."
        )));
    };

    let orchestrator = match get(raw, "orchestrator") {
        None => OrchestratorSpec::default(),
        Some(Value::Bool(required)) => OrchestratorSpec {
            required: *required,
            instruction: String::new(),
        },
        Some(Value::Object(orchestrator)) => OrchestratorSpec {
            required: parse_bool(
                orchestrator.get("required"),
                false,
                "agents.orchestrator.required",
                source,
            )?,
            instruction: text(orchestrator, "instruction"),
        },
        Some(_) => {
            return Err(Error::GameplanLoad(format!(
                "'agents.orchestrator' in {source} must be a bool or a mapping."
            )))
        }
    };

    let participants = match get(raw, "participants") {
        None => ParticipantSpec::default(),
        Some(Value::Object(participants)) => {
            let minimum = parse_positive_int(
                participants.get("min"),
                1,
                "agents.participants.min",
                source,
            )?;
            let maximum = match get(participants, "max") {
                None => None,
                Some(value) => Some(parse_positive_int(
                    Some(value),
                    1,
                    "agents.participants.max",
                    source,
                )?),
            };
            if maximum.is_some_and(|maximum| maximum < minimum) {
                return Err(Error::GameplanLoad(format!(
                    "'agents.participants.max' ({}) is below 'min' ({minimum}) in {source}.",
                    maximum.expect("checked just above")
                )));
            }
            ParticipantSpec {
                min: minimum,
                max: maximum,
            }
        }
        Some(_) => {
            return Err(Error::GameplanLoad(format!(
                "'agents.participants' in {source} must be a mapping."
            )))
        }
    };

    Ok(AgentsSpec {
        orchestrator,
        participants,
    })
}

fn parse_loop(raw: Option<&Value>, source: &str) -> Result<LoopSpec> {
    let Some(raw) = raw else {
        return Ok(LoopSpec::default());
    };
    let Value::Object(raw) = raw else {
        return Err(Error::GameplanLoad(format!(
            "'loop' in {source} must be a mapping."
        )));
    };

    let terminate: Vec<String> = match raw.get("terminate_on") {
        None => LoopSpec::default().terminate_on,
        Some(value) => {
            // A bare string is one keyword; `[]` is a harness that declares it
            // has no way to stop, which is worth saying at load rather than at
            // turn `max_turns`.
            let keywords: Vec<&Value> = match value {
                Value::String(_) => vec![value],
                Value::Array(items) if !items.is_empty() => items.iter().collect(),
                _ => {
                    return Err(Error::GameplanLoad(format!(
                        "'loop.terminate_on' in {source} must be a non-empty list — \
                         a harness with no terminator cannot end."
                    )))
                }
            };
            keywords
                .into_iter()
                .map(|keyword| pyfmt::str(keyword).trim().to_string())
                .filter(|keyword| !keyword.is_empty())
                .collect()
        }
    };
    if terminate.is_empty() {
        return Err(Error::GameplanLoad(format!(
            "'loop.terminate_on' in {source} contains no usable keywords."
        )));
    }

    Ok(LoopSpec {
        max_turns: parse_positive_int(raw.get("max_turns"), 50, "loop.max_turns", source)?,
        max_rounds: parse_positive_int(raw.get("max_rounds"), 3, "loop.max_rounds", source)?,
        terminate_on: terminate,
        phases: parse_phases(get(raw, "phases"), source)?,
        advance_on: raw
            .get("advance_on")
            .filter(|value| pyfmt::truthy(value))
            .map_or_else(
                || "NEXT_PHASE".to_string(),
                |value| pyfmt::str(value).trim().to_string(),
            ),
        orchestrator_retries: parse_count(
            raw.get("orchestrator_retries"),
            2,
            "loop.orchestrator_retries",
            source,
        )?,
        verdict_rethink: parse_bool(
            raw.get("verdict_rethink"),
            true,
            "loop.verdict_rethink",
            source,
        )?,
    })
}

fn parse_phases(raw: Option<&Value>, source: &str) -> Result<Vec<PhaseSpec>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let Value::Array(raw) = raw else {
        return Err(Error::GameplanLoad(format!(
            "'loop.phases' in {source} must be a list."
        )));
    };

    let mut phases: Vec<PhaseSpec> = Vec::with_capacity(raw.len());
    for entry in raw {
        let Value::Object(entry) = entry else {
            return Err(Error::GameplanLoad(format!(
                "Each entry in 'loop.phases' in {source} must be a mapping."
            )));
        };
        let name = text(entry, "name");
        if name.is_empty() {
            return Err(Error::GameplanLoad(format!(
                "A phase in {source} is missing 'name'."
            )));
        }
        if !SLUG_RE.is_match(&name) {
            return Err(Error::GameplanLoad(format!(
                "Phase name '{name}' in {source} must be a lowercase slug."
            )));
        }
        if phases.iter().any(|phase| phase.name == name) {
            return Err(Error::GameplanLoad(format!(
                "Duplicate phase name '{name}' in {source}."
            )));
        }
        phases.push(PhaseSpec {
            instruction: text(entry, "instruction"),
            rounds: parse_positive_int(
                entry.get("rounds"),
                1,
                &format!("loop.phases[{name}].rounds"),
                source,
            )?,
            rethink: parse_bool(
                entry.get("rethink"),
                false,
                &format!("loop.phases[{name}].rethink"),
                source,
            )?,
            name,
        });
    }
    Ok(phases)
}

/// Parse a tri-state name list: absent → `None`, `[]` → empty, list → those.
fn parse_name_list(raw: Option<&Value>, key: &str, source: &str) -> Result<Option<Vec<String>>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let items: Vec<&Value> = match raw {
        Value::String(_) => vec![raw],
        Value::Array(items) => items.iter().collect(),
        _ => {
            return Err(Error::GameplanLoad(format!(
                "'{key}' in {source} must be a list of names."
            )))
        }
    };
    let names: Vec<String> = items
        .into_iter()
        .map(|name| pyfmt::str(name).trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    if key == "tools" {
        let clashes: BTreeSet<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|name| RESERVED_TOOL_NAMES.contains(name))
            .collect();
        if !clashes.is_empty() {
            return Err(Error::GameplanLoad(format!(
                "'{key}' in {source} names reserved tool(s) {}; the runtime supplies them.",
                clashes.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
    }
    Ok(Some(names))
}

fn parse_result(raw: Option<&Value>, source: &str) -> Result<Vec<ResultField>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let Value::Object(raw) = raw else {
        return Err(Error::GameplanLoad(format!(
            "'result' in {source} must be a mapping."
        )));
    };

    let mut fields: Vec<ResultField> = Vec::with_capacity(raw.len());
    for (name, value) in raw {
        let field_name = name.trim();
        if field_name.is_empty() {
            continue;
        }
        let (type_name, description) = match value {
            Value::String(spelling) => (spelling.trim().to_string(), String::new()),
            Value::Object(entry) => {
                let type_name = entry
                    .get("type")
                    .filter(|value| pyfmt::truthy(value))
                    .map_or_else(
                        || "str".to_string(),
                        |value| pyfmt::str(value).trim().to_string(),
                    );
                (type_name, text(entry, "description"))
            }
            _ => {
                return Err(Error::GameplanLoad(format!(
                    "'result.{field_name}' in {source} must be a type name or a mapping."
                )))
            }
        };
        if result_type(&type_name).is_none() {
            let known: Vec<&str> = RESULT_TYPES.iter().map(|(spelling, _)| *spelling).collect();
            return Err(Error::GameplanLoad(format!(
                "'result.{field_name}' in {source} has unknown type '{type_name}'. Known: {}.",
                known.join(", ")
            )));
        }
        fields.push(ResultField {
            name: field_name.to_string(),
            type_name,
            description,
        });
    }
    Ok(fields)
}

fn parse_positive_int(raw: Option<&Value>, default: i64, key: &str, source: &str) -> Result<i64> {
    let Some(value) = raw else {
        return Ok(default);
    };
    let parsed = coerce_int(value).ok_or_else(|| {
        Error::GameplanLoad(format!(
            "'{key}' in {source} must be an integer, got {}.",
            pyfmt::repr(value)
        ))
    })?;
    if parsed < 1 {
        return Err(Error::GameplanLoad(format!(
            "'{key}' in {source} must be >= 1, got {parsed}."
        )));
    }
    Ok(parsed)
}

/// Like [`parse_positive_int`], but zero is a legitimate answer.
///
/// `orchestrator_retries: 0` means "do not re-ask", which is a real choice for
/// a harness that would rather end than nag.
fn parse_count(raw: Option<&Value>, default: i64, key: &str, source: &str) -> Result<i64> {
    let Some(value) = raw else {
        return Ok(default);
    };
    let parsed = coerce_int(value).ok_or_else(|| {
        Error::GameplanLoad(format!(
            "'{key}' in {source} must be an integer, got {}.",
            pyfmt::repr(value)
        ))
    })?;
    if parsed < 0 {
        return Err(Error::GameplanLoad(format!(
            "'{key}' in {source} must be >= 0, got {parsed}."
        )));
    }
    Ok(parsed)
}

/// Require an actual YAML boolean instead of truth-testing arbitrary data.
fn parse_bool(raw: Option<&Value>, default: bool, key: &str, source: &str) -> Result<bool> {
    match raw {
        None => Ok(default),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(value) => Err(Error::GameplanLoad(format!(
            "'{key}' in {source} must be a boolean, got {}.",
            pyfmt::repr(value)
        ))),
    }
}

/// An integer from a number or a decimal string, and nothing else.
///
/// A bool is refused rather than counted as 0 or 1, so `max_rounds: true`
/// cannot quietly mean one round.
fn coerce_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float.trunc() as i64)),
        Value::String(text) => parse_int_literal(text),
        _ => None,
    }
}

/// The integer literal a frontmatter string may spell: surrounding space, a
/// sign, then digits that may be grouped with single underscores.
fn parse_int_literal(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') {
        return None;
    }
    if digits.contains("__") || !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let magnitude = digits.replace('_', "").parse::<i64>().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// A mapping entry, treating an explicit `null` as absent.
///
/// Only for the keys whose default is "the structure was not declared" —
/// `agents: ~` means the same as no `agents:` at all. Keys with a scalar
/// default read `Map::get` directly, because `verdict_rethink: ~` is a
/// declared value of the wrong type rather than an undeclared key.
fn get<'a>(data: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    data.get(key).filter(|value| !value.is_null())
}

/// `str(data.get(key, "") or "").strip()`.
fn text(data: &Map<String, Value>, key: &str) -> String {
    data.get(key)
        .filter(|value| pyfmt::truthy(value))
        .map(|value| pyfmt::str(value).trim().to_string())
        .unwrap_or_default()
}

/// The type name used by the one error that reports it.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(data: Value) -> Result<HarnessSpec> {
        parse_harness(&data, "test.md")
    }

    fn ok(data: Value) -> HarnessSpec {
        parse(data).expect("valid harness")
    }

    fn message(data: Value) -> String {
        parse(data).expect_err("invalid harness").to_string()
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_orchestrator_key_takes_two_shapes_and_refuses_a_third() {
        assert!(!ok(json!({})).agents.orchestrator.required);
        assert!(
            ok(json!({"agents": {"orchestrator": true}}))
                .agents
                .orchestrator
                .required
        );

        let spec = ok(json!({
            "agents": {"orchestrator": {"required": true, "instruction": "Be brief."}}
        }));
        assert!(spec.agents.orchestrator.required);
        assert_eq!(spec.agents.orchestrator.instruction, "Be brief.");

        assert!(message(json!({"agents": {"orchestrator": 3}})).contains("bool or a mapping"));
    }

    #[test]
    fn participant_bounds_default_open_and_parse_when_given() {
        // An absent `max` is unbounded, not zero — the difference between a
        // harness that seats anyone and one that seats nobody.
        assert_eq!(
            ok(json!({})).agents.participants,
            ParticipantSpec::default()
        );
        assert_eq!(
            ok(json!({"agents": {"participants": {"min": 2, "max": 5}}}))
                .agents
                .participants,
            ParticipantSpec {
                min: 2,
                max: Some(5)
            }
        );
    }

    #[test]
    fn unsatisfiable_bounds_are_rejected() {
        assert!(
            message(json!({"agents": {"participants": {"min": 4, "max": 2}}}))
                .contains("below 'min'")
        );
        assert!(message(json!({"agents": {"participants": {"min": 0}}})).contains(">= 1"));
    }

    #[test]
    fn loop_defaults_include_the_judges_rethink() {
        let loop_spec = ok(json!({})).loop_spec;
        assert_eq!(loop_spec.terminate_on, names(&["END_SESSION"]));
        assert_eq!(loop_spec.max_rounds, 3);
        assert!(loop_spec.phases.is_empty());
        assert!(loop_spec.verdict_rethink);

        assert!(
            !ok(json!({"loop": {"verdict_rethink": false}}))
                .loop_spec
                .verdict_rethink
        );
    }

    #[test]
    fn a_single_terminator_is_wrapped_and_none_at_all_is_refused() {
        assert_eq!(
            ok(json!({"loop": {"terminate_on": "DONE"}}))
                .loop_spec
                .terminate_on,
            names(&["DONE"])
        );
        assert!(message(json!({"loop": {"terminate_on": []}})).contains("cannot end"));
        assert!(message(json!({"loop": {"terminate_on": ["  "]}})).contains("no usable keywords"));
    }

    #[test]
    fn the_consensus_keyword_is_recognised_only_when_declared() {
        let spec = ok(json!({"loop": {"terminate_on": ["END_SESSION", "CONSENSUS_REACHED"]}}));
        assert_eq!(
            spec.loop_spec.consensus_keyword(),
            Some("CONSENSUS_REACHED")
        );
        assert_eq!(
            ok(json!({"loop": {"terminate_on": ["END_SESSION"]}}))
                .loop_spec
                .consensus_keyword(),
            None
        );
    }

    #[test]
    fn a_scalar_of_the_wrong_type_is_rejected_not_coerced() {
        // A bare bool has to be caught too, or it would count as 0 or 1; and
        // a quoted "false" is a string that must not become true merely
        // because non-empty strings are truthy.
        assert!(message(json!({"loop": {"max_rounds": "many"}})).contains("must be an integer"));
        assert!(message(json!({"loop": {"max_rounds": true}})).contains("must be an integer"));
        assert!(
            message(json!({"loop": {"verdict_rethink": "false"}})).contains("must be a boolean")
        );
    }

    #[test]
    fn phase_fields_default_to_one_round_and_no_rethink() {
        let spec = ok(json!({"loop": {"phases": [
            {"name": "think", "instruction": "Alone.", "rounds": 2},
            {"name": "rethink", "rethink": true},
        ]}}));
        assert_eq!(
            spec.loop_spec.phases[0],
            PhaseSpec {
                name: "think".into(),
                instruction: "Alone.".into(),
                rounds: 2,
                rethink: false,
            }
        );
        assert!(spec.loop_spec.phases[1].rethink);
        assert_eq!(spec.loop_spec.phases[1].rounds, 1);
    }

    #[test]
    fn a_malformed_phase_list_is_rejected() {
        for (phases, expected) in [
            (json!([{"instruction": "x"}]), "missing 'name'"),
            (json!([{"name": "a"}, {"name": "a"}]), "Duplicate phase"),
            (json!([{"name": "Think Hard"}]), "lowercase slug"),
            (json!({"name": "think"}), "must be a list"),
            (
                json!([{"name": "think", "rethink": "false"}]),
                "must be a boolean",
            ),
        ] {
            let reported = message(json!({"loop": {"phases": phases}}));
            assert!(reported.contains(expected), "{reported}");
        }
    }

    #[test]
    fn the_tools_key_narrows_what_was_registered() {
        // Absent means all, empty means none, named means those — in
        // registration order, not in the order the harness happened to list.
        assert_eq!(
            ok(json!({}))
                .resolve_tools(&names(&["cmd", "read_file"]))
                .expect("all"),
            names(&["cmd", "read_file"])
        );
        assert!(ok(json!({"tools": []}))
            .resolve_tools(&names(&["cmd"]))
            .expect("none")
            .is_empty());

        let spec = ok(json!({"tools": ["list_dir", "cmd"]}));
        assert_eq!(
            spec.resolve_tools(&names(&["cmd", "read_file", "list_dir"]))
                .expect("narrowed"),
            names(&["cmd", "list_dir"])
        );
    }

    #[test]
    fn an_unknown_tool_is_an_error_not_a_silent_drop() {
        let spec = ok(json!({"name": "x", "tools": ["teleport"]}));
        let error = spec
            .resolve_tools(&names(&["cmd"]))
            .expect_err("unregistered");
        assert!(error.to_string().contains("teleport"), "{error}");
    }

    #[test]
    fn a_reserved_tool_name_is_rejected_at_load() {
        assert!(message(json!({"tools": ["Skill"]})).contains("reserved"));
    }

    #[test]
    fn the_skills_key_unions_with_the_session_and_dedupes() {
        let spec = ok(json!({"skills": ["challenge", "summarize"]}));
        assert_eq!(
            spec.resolve_skills(&names(&["summarize"])),
            names(&["summarize", "challenge"])
        );
        assert_eq!(
            spec.resolve_skills(&names(&["summarize", "summarize"])),
            names(&["summarize", "challenge"])
        );
        assert_eq!(ok(json!({})).resolve_skills(&names(&["a"])), names(&["a"]));
    }

    #[test]
    fn a_result_field_takes_the_shorthand_or_the_long_form() {
        let spec = ok(json!({"result": {"summary": "str"}}));
        assert_eq!(spec.result[0].name, "summary");
        assert_eq!(spec.result[0].result_type(), ResultType::Str);

        let spec = ok(json!({"result": {"ok": {"type": "bool", "description": "Did it work."}}}));
        assert_eq!(spec.result[0].result_type(), ResultType::Bool);
        assert_eq!(spec.result[0].description, "Did it work.");

        assert!(message(json!({"result": {"x": "widget"}})).contains("unknown type"));
    }

    #[test]
    fn a_name_is_optional_but_must_be_a_slug_when_given() {
        assert_eq!(ok(json!({})).name, "");
        assert!(message(json!({"name": "My Gameplan"})).contains("lowercase slug"));
    }

    #[test]
    fn a_passing_session_returns_the_allowed_tools() {
        let spec = ok(json!({"name": "t", "tools": ["cmd"]}));
        let tools = validate_harness(
            &spec,
            &names(&["A", "B"]),
            Some("M"),
            &names(&["cmd", "read_file"]),
        )
        .expect("valid session");
        assert_eq!(tools, names(&["cmd"]));
    }

    #[test]
    fn every_problem_with_the_roster_is_reported_at_once() {
        // One run, one list — not one error per re-run.
        let spec = ok(json!({
            "name": "t",
            "agents": {"orchestrator": true, "participants": {"min": 4}},
            "tools": ["teleport"],
        }));
        let error = validate_harness(&spec, &names(&["A", "A"]), None, &names(&["cmd"]))
            .expect_err("four problems");
        let reported = error.to_string();

        for expected in [
            "requires an orchestrator",
            "at least 4",
            "duplicate agent name",
            "teleport",
        ] {
            assert!(reported.contains(expected), "{reported}");
        }
    }

    #[test]
    fn a_name_clash_with_the_orchestrator_is_a_problem_too() {
        // Routing is by name, so a clash makes an `@Name` ambiguous.
        let spec = ok(json!({"name": "t"}));
        let error =
            validate_harness(&spec, &names(&["A"]), Some("A"), &[]).expect_err("name clash");
        assert!(error.to_string().contains("shares a name"), "{error}");
    }

    #[test]
    fn the_roster_must_fit_the_declared_bounds() {
        let spec = ok(json!({"name": "t", "agents": {"participants": {"min": 3}}}));
        let error = validate_harness(&spec, &names(&["A"]), None, &[]).expect_err("too few");
        assert!(error.to_string().contains("at least 3"), "{error}");

        let spec = ok(json!({"name": "t", "agents": {"participants": {"max": 2}}}));
        let error =
            validate_harness(&spec, &names(&["A", "B", "C"]), None, &[]).expect_err("too many");
        assert!(error.to_string().contains("at most 2"), "{error}");
    }

    #[test]
    fn a_bare_spec_is_usable() {
        let spec = HarnessSpec::default();
        assert_eq!(spec.loop_spec.terminate_on, names(&["END_SESSION"]));
        assert_eq!(
            spec.resolve_tools(&names(&["cmd"])).expect("all"),
            names(&["cmd"])
        );
        assert!(spec.resolve_skills(&[]).is_empty());
    }
}
