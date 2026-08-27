//! Progressive disclosure for skills.
//!
//! An agent's system prompt carries one line per skill — name and description —
//! and nothing else. When the agent decides it needs a skill it calls the
//! built-in `Skill` tool, and the body comes back as a tool result.
//!
//! Why a tool result rather than a system-prompt injection: both native APIs
//! prefix-cache system prompts, so mutating one mid-session invalidates the
//! cache for every subsequent call. A tool result is also the semantically
//! correct slot, and it lands in the calling agent's private scratch buffer, so
//! no other agent pays for a skill it did not ask for.
//!
//! **The cost of that choice, stated plainly:** the body lives for the current
//! agent turn only. A later turn that needs the same skill invokes it again and
//! pays again. Repeated invocation *within* one turn is free — the second call
//! says so instead of repeating the body.
//!
//! The alternative — concatenating every attached skill's complete body into
//! every agent's system prompt — costs far more. The built-in `agent-browser`
//! skill is 250 lines; an agent holding it would pay those 250 lines on every
//! turn of the session whether or not a browser was ever opened.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::error::{Error, Result};
use crate::pyfmt;
use crate::skill::loader::SkillConfig;
use crate::tooling::{Arguments, ToolSpec};

/// The built-in tool name skills are loaded through.
///
/// Reserved: registering a tool by this name is an error, because it would
/// shadow skill loading with no diagnostic.
pub const SKILL_TOOL_NAME: &str = "Skill";

/// Called with a skill's bundled directories when the grant is permitted.
pub type GrantPaths = Arc<dyn Fn(&[PathBuf]) + Send + Sync>;

/// Returns the skills one agent may load.
pub type SkillsFor = Arc<dyn Fn(&str) -> Vec<SkillConfig> + Send + Sync>;

fn index_header() -> String {
    format!(
        "\n## Available Skills\n\
         Call the `{SKILL_TOOL_NAME}` tool with {{\"name\": \"<skill-name>\"}} \
         to load a skill's full instructions before you use it. Only the \
         descriptions are shown here.\n"
    )
}

/// Render the one-line-per-skill index for a system prompt.
///
/// Empty when the agent has no skills, so a prompt does not carry a heading
/// introducing nothing.
pub fn format_skills_index(skills: &[SkillConfig]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(skills.len() + 1);
    lines.push(index_header());
    lines.extend(
        skills
            .iter()
            .map(|skill| format!("- {}: {}", skill.name, skill.description)),
    );
    lines.join("\n")
}

/// What one agent has loaded during the turn it is currently taking.
///
/// A fresh instance per turn is what makes "the body lives for this turn only"
/// true rather than aspirational.
///
/// The load bookkeeping sits behind a lock because the `Skill` tool's handler
/// and the runtime reading the gate hold the same activation at the same time.
pub struct SkillActivation {
    skills: BTreeMap<String, SkillConfig>,
    grant_paths: Option<GrantPaths>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    loaded: BTreeSet<String>,
    gate: Option<BTreeSet<String>>,
}

impl SkillActivation {
    pub fn new(skills: BTreeMap<String, SkillConfig>, grant_paths: Option<GrantPaths>) -> Self {
        SkillActivation {
            skills,
            grant_paths,
            state: Mutex::new(State::default()),
        }
    }

    /// The skills this agent may load, sorted.
    pub fn names(&self) -> Vec<String> {
        self.skills.keys().cloned().collect()
    }

    /// Tool names permitted by the skills active this turn.
    ///
    /// `None` means no active skill narrows anything, which is not the same as
    /// the empty set — a skill declaring `allowed-tools: []` genuinely permits
    /// nothing.
    pub fn gate(&self) -> Option<BTreeSet<String>> {
        self.state
            .lock()
            .expect("activation lock poisoned")
            .gate
            .clone()
    }

    /// Activate a skill and return what the agent should read: the body, plus a
    /// bundle manifest when it ships one.
    ///
    /// An unknown skill is a session error rather than a panic, because the
    /// dispatcher turns it into an error result the model can recover from.
    pub fn load(&self, name: &str) -> Result<String> {
        let Some(skill) = self.skills.get(name) else {
            let available = self.names().join(", ");
            let available = if available.is_empty() {
                "(none)".to_string()
            } else {
                available
            };
            return Err(Error::session(format!(
                "Unknown skill '{name}'. Available to you: {available}"
            )));
        };

        {
            let mut state = self.state.lock().expect("activation lock poisoned");
            if !state.loaded.insert(name.to_string()) {
                return Ok(format!(
                    "[Skill:{name}] Already loaded earlier in this turn; see above."
                ));
            }
            narrow(&mut state, skill);
        }
        Ok(skill.content.clone() + &self.manifest(skill))
    }

    /// Render the bundled-file list, granting read access if permitted.
    fn manifest(&self, skill: &SkillConfig) -> String {
        let bundles = skill.bundle_paths();
        if bundles.is_empty() {
            return String::new();
        }
        let Some(grant) = self.grant_paths.as_ref().filter(|_| skill.builtin) else {
            let names: Vec<String> = bundles.iter().map(|path| directory_name(path)).collect();
            return format!(
                "\n\n[Skill:{}] This skill bundles {}, but access was not \
                 granted. Reading those files will require approval.",
                skill.name,
                names.join(", ")
            );
        };
        grant(&bundles);
        let listed: Vec<String> = bundles
            .iter()
            .map(|path| format!("- {}", path.display()))
            .collect();
        format!(
            "\n\n[Skill:{}] Bundled resources:\n{}",
            skill.name,
            listed.join("\n")
        )
    }
}

/// Fold a skill's `allowed-tools` into this turn's gate.
///
/// The gate is the *union* across skills activated this turn. Two skills each
/// naming a different tool must leave both callable, otherwise loading a second
/// skill would silently disable the first.
fn narrow(state: &mut State, skill: &SkillConfig) {
    let Some(allowed) = &skill.allowed_tools else {
        return;
    };
    state
        .gate
        .get_or_insert_with(BTreeSet::new)
        .extend(allowed.iter().cloned());
}

fn directory_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Owns per-agent skill sets and builds the `Skill` tool.
#[derive(Clone)]
pub struct SkillRegistry {
    skills_for: SkillsFor,
    /// Called with bundled directories on activation. `None` disables the grant
    /// entirely, so bundles are listed but not opened.
    grant_paths: Option<GrantPaths>,
}

impl SkillRegistry {
    pub fn new(skills_for: SkillsFor, grant_paths: Option<GrantPaths>) -> Self {
        SkillRegistry {
            skills_for,
            grant_paths,
        }
    }

    /// Start a fresh activation for one agent turn.
    pub fn activation_for(&self, agent_name: &str) -> Arc<SkillActivation> {
        let skills = (self.skills_for)(agent_name)
            .into_iter()
            .map(|skill| (skill.name.clone(), skill))
            .collect();
        Arc::new(SkillActivation::new(skills, self.grant_paths.clone()))
    }

    /// Build the `Skill` tool bound to one turn's activation.
    ///
    /// `None` when the agent has no skills — an enum with no members is not a
    /// valid schema, and advertising a tool that can only fail is worse than
    /// not advertising it.
    pub fn build_tool(&self, activation: &Arc<SkillActivation>) -> Option<ToolSpec> {
        let names = activation.names();
        if names.is_empty() {
            return None;
        }
        let bound = Arc::clone(activation);
        Some(ToolSpec::new(
            SKILL_TOOL_NAME,
            "Load a skill's full instructions. Call this before acting on a \
             skill you have only seen the description of.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": names,
                        "description": "The skill to load.",
                    }
                },
                "required": ["name"],
            }),
            Arc::new(move |arguments: &Arguments, _actor: &str| {
                bound.load(&arguments.get("name").map(pyfmt::str).unwrap_or_default())
            }),
        ))
    }
}

/// Narrow a toolkit to what the active skills permit.
///
/// **Restrictive only.** A skill can never grant a tool the agent's toolkit
/// lacks, so this intersects rather than unions. `Skill` itself is never gated:
/// an agent that loaded a narrow skill must still be able to load another one.
pub fn apply_gate(tools: &[ToolSpec], gate: Option<&BTreeSet<String>>) -> Vec<ToolSpec> {
    let Some(gate) = gate else {
        return tools.to_vec();
    };
    tools
        .iter()
        .filter(|tool| gate.contains(&tool.name) || tool.name == SKILL_TOOL_NAME)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> SkillConfig {
        SkillConfig {
            name: name.into(),
            description: "Does a thing.".into(),
            content: "BODY".into(),
            ..SkillConfig::default()
        }
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec::new(
            name,
            name,
            json!({"type": "object", "properties": {}}),
            Arc::new(|_: &Arguments, _: &str| Ok(String::new())),
        )
    }

    fn activation(skills: Vec<SkillConfig>) -> Arc<SkillActivation> {
        SkillRegistry::new(Arc::new(move |_: &str| skills.clone()), None).activation_for("")
    }

    fn gate_of(activation: &SkillActivation) -> Option<Vec<String>> {
        activation
            .gate()
            .map(|gate| gate.into_iter().collect::<Vec<_>>())
    }

    #[test]
    fn the_index_names_the_bodies_without_carrying_them() {
        // Names and descriptions only, plus how to fetch the rest — the whole
        // point of progressive disclosure.
        let mut alpha = skill("a");
        alpha.description = "Alpha.".into();
        let mut beta = skill("b");
        beta.description = "Beta.".into();

        let index = format_skills_index(&[alpha, beta]);
        assert!(index.contains("- a: Alpha."), "{index}");
        assert!(index.contains("- b: Beta."), "{index}");
        assert!(!index.contains("BODY"), "{index}");
        assert!(index.contains(SKILL_TOOL_NAME), "{index}");
    }

    #[test]
    fn no_skills_renders_nothing() {
        assert_eq!(format_skills_index(&[]), "");
    }

    #[test]
    fn the_body_is_served_once_per_turn() {
        // A reload inside one turn is answered without repeating the text, but
        // the body is scoped to that turn — the next one pays for it again.
        let act = activation(vec![skill("a")]);
        assert_eq!(act.load("a").expect("loads"), "BODY");

        let second = act.load("a").expect("loads");
        assert!(!second.contains("BODY"), "{second}");
        assert!(second.contains("Already loaded"), "{second}");

        assert_eq!(
            activation(vec![skill("a")]).load("a").expect("loads"),
            "BODY"
        );
    }

    #[test]
    fn an_unavailable_skill_names_what_is_available() {
        let error = activation(vec![skill("a"), skill("b")])
            .load("nope")
            .expect_err("no such skill");
        assert_eq!(
            error.to_string(),
            "Unknown skill 'nope'. Available to you: a, b"
        );

        let error = activation(Vec::new())
            .load("a")
            .expect_err("no skills at all");
        assert!(error.to_string().contains("(none)"), "{error}");
    }

    #[test]
    fn only_a_skill_declaring_allowed_tools_narrows() {
        let act = activation(vec![skill("a")]);
        assert_eq!(gate_of(&act), None);
        act.load("a").expect("loads");
        assert_eq!(gate_of(&act), None);
    }

    #[test]
    fn an_explicit_empty_list_permits_nothing() {
        // `allowed-tools: []` is a real answer, not the same as absent.
        let act = activation(vec![SkillConfig {
            allowed_tools: Some(Vec::new()),
            ..skill("a")
        }]);
        act.load("a").expect("loads");
        assert_eq!(gate_of(&act), Some(Vec::new()));
    }

    #[test]
    fn two_skills_union_rather_than_intersect() {
        // Loading a second skill must not silently disable the first.
        let act = activation(vec![
            SkillConfig {
                allowed_tools: Some(vec!["read_file".into()]),
                ..skill("a")
            },
            SkillConfig {
                allowed_tools: Some(vec!["cmd".into()]),
                ..skill("b")
            },
        ]);
        act.load("a").expect("loads");
        assert_eq!(gate_of(&act), Some(vec!["read_file".to_string()]));
        act.load("b").expect("loads");
        assert_eq!(
            gate_of(&act),
            Some(vec!["cmd".to_string(), "read_file".to_string()])
        );
    }

    #[test]
    fn apply_gate_is_restrictive_only() {
        // A skill can never grant a tool the agent's toolkit lacks, and no gate
        // at all passes the toolkit through untouched.
        let gate: BTreeSet<String> = ["read_file", "cmd"].iter().map(|s| s.to_string()).collect();
        let narrowed = apply_gate(&[tool("read_file")], Some(&gate));
        assert_eq!(
            narrowed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["read_file"]
        );

        let tools = vec![tool("cmd"), tool("read_file")];
        assert_eq!(apply_gate(&tools, None), tools);
    }

    #[test]
    fn the_skill_tool_is_never_gated_out() {
        // An agent under a narrow skill must still be able to load another.
        let tools = [tool("cmd"), tool(SKILL_TOOL_NAME)];
        let narrowed = apply_gate(&tools, Some(&BTreeSet::new()));
        assert_eq!(
            narrowed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            [SKILL_TOOL_NAME]
        );
    }

    #[test]
    fn the_enum_is_this_agents_skills_and_the_handler_loads_them() {
        let registry = SkillRegistry::new(Arc::new(|_: &str| vec![skill("a"), skill("b")]), None);
        let act = registry.activation_for("Alice");
        let spec = registry.build_tool(&act).expect("two skills");

        assert_eq!(
            spec.parameters["properties"]["name"]["enum"],
            json!(["a", "b"])
        );

        let mut arguments = Arguments::new();
        arguments.insert("name".into(), json!("a"));
        assert_eq!(
            crate::tooling::ToolHandler::call(&*spec.handler, &arguments, ""),
            Ok("BODY".to_string())
        );
    }

    #[test]
    fn no_tool_when_the_agent_has_no_skills() {
        // An empty enum is not a valid schema, and the tool could only fail.
        let registry = SkillRegistry::new(Arc::new(|_: &str| Vec::new()), None);
        let act = registry.activation_for("Alice");
        assert!(registry.build_tool(&act).is_none());
    }

    #[test]
    fn the_registry_resolves_per_agent() {
        let registry = SkillRegistry::new(
            Arc::new(|name: &str| vec![skill(&name.to_lowercase())]),
            None,
        );
        assert_eq!(registry.activation_for("Alice").names(), ["alice"]);
        assert_eq!(registry.activation_for("Bob").names(), ["bob"]);
    }

    struct TempDir(PathBuf);

    impl TempDir {
        /// A `demo/` skill directory shipping a `scripts/` bundle.
        fn bundled(tag: &str, builtin: bool) -> (Self, SkillConfig) {
            let root = std::env::temp_dir().join(format!("kerness-skillrt-{tag}"));
            let _ = std::fs::remove_dir_all(&root);
            let base = root.join("demo");
            std::fs::create_dir_all(base.join("scripts")).expect("create bundle");
            std::fs::write(base.join("scripts").join("run.sh"), "echo hi\n").expect("write script");
            let config = SkillConfig {
                base_dir: Some(base),
                builtin,
                ..skill("demo")
            };
            (TempDir(root), config)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn recording() -> (GrantPaths, Arc<Mutex<Vec<PathBuf>>>) {
        let granted = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&granted);
        let grant: GrantPaths = Arc::new(move |paths: &[PathBuf]| {
            sink.lock().expect("sink lock").extend_from_slice(paths);
        });
        (grant, granted)
    }

    fn with_grant(config: SkillConfig, grant: GrantPaths) -> Arc<SkillActivation> {
        SkillRegistry::new(Arc::new(move |_: &str| vec![config.clone()]), Some(grant))
            .activation_for("")
    }

    #[test]
    fn a_builtin_bundle_is_listed_and_granted() {
        let (_dir, config) = TempDir::bundled("granted", true);
        let (grant, granted) = recording();
        let result = with_grant(config, grant).load("demo").expect("loads");

        assert!(result.contains("Bundled resources:"), "{result}");
        let granted = granted.lock().expect("sink lock");
        assert_eq!(
            granted
                .iter()
                .map(|path| directory_name(path))
                .collect::<Vec<_>>(),
            ["scripts"]
        );
    }

    #[test]
    fn an_untrusted_bundle_is_listed_but_not_granted() {
        // Activating a skill from an arbitrary path must not widen access.
        let (_dir, config) = TempDir::bundled("untrusted", false);
        let (grant, granted) = recording();
        let result = with_grant(config, grant).load("demo").expect("loads");

        assert!(result.contains("access was not granted"), "{result}");
        assert!(granted.lock().expect("sink lock").is_empty());
    }

    #[test]
    fn no_manifest_when_there_is_no_bundle() {
        let root = std::env::temp_dir().join("kerness-skillrt-plain");
        std::fs::create_dir_all(&root).expect("create dir");
        let config = SkillConfig {
            base_dir: Some(root.clone()),
            builtin: true,
            ..skill("plain")
        };
        let act = activation(vec![config]);
        assert_eq!(act.load("plain").expect("loads"), "BODY");
        let _ = std::fs::remove_dir_all(&root);
    }
}
