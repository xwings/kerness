//! Gameplan file loading.
//!
//! A gameplan is a Markdown file whose YAML frontmatter *is* the harness
//! definition and whose body is the orchestrator's instruction manual. This
//! module resolves the file, splits it, and hands the frontmatter to
//! [`crate::harness`].

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::assets;
use crate::error::{Error, Result};
use crate::harness::{parse_harness, HarnessSpec};
use crate::yaml;

/// A loaded gameplan: the harness contract plus the instruction body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameplanConfig {
    pub name: String,
    pub harness: HarnessSpec,
    /// The Markdown body with frontmatter removed — what the orchestrator reads.
    pub body: String,
    /// The complete file text, frontmatter included.
    pub raw_text: String,
    /// Absolute path the gameplan was read from. Kept so the session can
    /// resolve sibling assets — a third-party project ships a gameplan and its
    /// personas in one directory, and the paths inside that gameplan should
    /// mean what they say regardless of where the process was started.
    pub path: String,
}

impl GameplanConfig {
    /// The directory the gameplan lives in, for resolving its siblings.
    pub fn directory(&self) -> Option<PathBuf> {
        if self.path.is_empty() {
            return None;
        }
        Some(Path::new(&self.path).parent().unwrap_or(Path::new("")).to_path_buf())
    }

    /// Shorthand for the harness role contract.
    pub fn requires_orchestrator(&self) -> bool {
        self.harness.agents.orchestrator.required
    }

    /// Shorthand for `harness.loop_spec.max_rounds`.
    pub fn max_rounds(&self) -> i64 {
        self.harness.loop_spec.max_rounds
    }
}

/// The built-in gameplan directory.
fn gameplans_dir() -> PathBuf {
    assets::root().join("gameplans")
}

/// Load a gameplan from a built-in name or a file path.
///
/// If *name_or_path* looks like a path — it contains a separator or ends with
/// `.md` — it is resolved as-is first, then relative to the built-in
/// `gameplans/` directory. Otherwise it names a built-in gameplan.
pub fn load_gameplan(name_or_path: &str) -> Result<GameplanConfig> {
    let path = resolve_gameplan_path(name_or_path)?;

    let text = std::fs::read_to_string(&path).map_err(|err| {
        Error::GameplanLoad(format!("Cannot read gameplan file {}: {err}", path.display()))
    })?;

    let source = path.display().to_string();
    let (frontmatter, body) = split_frontmatter(&text, &source)?;
    let body = body.unwrap_or(&text);

    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();

    // The declared name is validated as a slug; a name derived from the
    // filename is not, because the user did not choose it as an identifier.
    let mut harness = parse_harness(&frontmatter, &source)?;
    if harness.name.is_empty() {
        harness.name = stem.clone();
    }

    Ok(GameplanConfig {
        name: if harness.name.is_empty() {
            stem
        } else {
            harness.name.clone()
        },
        harness,
        body: body.trim().to_string(),
        path: std::fs::canonicalize(&path)
            .unwrap_or(path)
            .display()
            .to_string(),
        raw_text: text,
    })
}

/// The names of all built-in gameplans.
pub fn list_builtin_gameplans() -> Vec<String> {
    assets::list_markdown_stems(&gameplans_dir())
}

/// Split YAML frontmatter from the Markdown body.
///
/// The body comes back as `None` when the file has no frontmatter, which loads
/// on harness defaults with the whole file as the instruction body.
fn split_frontmatter<'a>(text: &'a str, source: &str) -> Result<(Value, Option<&'a str>)> {
    static FRONTMATTER_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)\A---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\z)").expect("static pattern")
    });

    let Some(captures) = FRONTMATTER_RE.captures(text) else {
        return Ok((Value::Object(Default::default()), None));
    };
    let raw = captures.get(1).expect("group 1 always participates").as_str();
    let body = &text[captures.get(0).expect("the whole match").end()..];

    let data = yaml::parse(raw)
        .map_err(|err| Error::GameplanLoad(format!("Invalid YAML frontmatter in {source}: {err}")))?;

    let data = if data.is_null() {
        Value::Object(Default::default())
    } else {
        data
    };
    if !data.is_object() {
        return Err(Error::GameplanLoad(format!(
            "Frontmatter in {source} must be a mapping, got {}.",
            python_type_name(&data)
        )));
    }
    Ok((data, Some(body)))
}

fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        _ => "NoneType",
    }
}

/// Resolve a gameplan name or path to an existing file.
///
/// A name that looks like a path is tried verbatim, then under the built-in
/// directory; anything else is a built-in name.
fn resolve_gameplan_path(name_or_path: &str) -> Result<PathBuf> {
    let is_path =
        name_or_path.contains('/') || name_or_path.contains('\\') || name_or_path.ends_with(".md");

    if is_path {
        let candidate = PathBuf::from(name_or_path);
        if candidate.exists() {
            return Ok(candidate);
        }
        let candidate = gameplans_dir().join(name_or_path);
        if candidate.exists() {
            return Ok(candidate);
        }
        return Err(Error::GameplanLoad(format!(
            "Gameplan file not found: {name_or_path}"
        )));
    }

    let path = gameplans_dir().join(format!("{name_or_path}.md"));
    if !path.exists() {
        return Err(Error::GameplanLoad(format!(
            "Gameplan file not found: {}",
            path.display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::ResultType;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(tag: &str, text: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-gameplan-{tag}.md"));
            std::fs::write(&path, text).expect("write gameplan");
            TempFile(path)
        }

        fn name(&self) -> String {
            self.0.display().to_string()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn the_body_is_the_prose_and_raw_text_is_the_whole_file() {
        // The body is what the orchestrator reads; YAML must not leak in.
        let config = load_gameplan("debate").expect("built-in loads");

        assert_eq!(config.name, "debate");
        assert!(config.requires_orchestrator());
        assert_eq!(config.max_rounds(), 3);

        assert!(config.body.contains("# Debate"));
        assert!(!config.body.lines().next().expect("a body").contains("---"));
        assert!(!config.body.contains("terminate_on"));

        assert!(config.raw_text.starts_with("---"));
        assert!(config.raw_text.contains("name: debate"));
    }

    #[test]
    fn a_missing_gameplan_is_named_in_the_error() {
        for missing in ["nonexistent_gameplan", "/nonexistent/custom.md"] {
            let error = load_gameplan(missing).expect_err("no such gameplan");
            assert!(error.to_string().contains("not found"), "{error}");
        }
    }

    #[test]
    fn every_discovered_gameplan_loads_under_its_own_name() {
        // Enumerated rather than listed, so a fourth bundled gameplan cannot
        // ship without this check applying to it.
        let names = list_builtin_gameplans();
        for expected in ["debate", "discussion", "research"] {
            assert!(names.iter().any(|name| name == expected), "{names:?}");
        }
        for name in &names {
            let config = load_gameplan(name).expect("built-in loads");
            assert_eq!(&config.name, name);
            assert!(config.requires_orchestrator());
        }
    }

    #[test]
    fn debates_bounds_terminators_and_result_shape_are_read() {
        let harness = load_gameplan("debate").expect("built-in loads").harness;

        assert_eq!(harness.agents.participants.min, 2);
        assert_eq!(harness.agents.participants.max, Some(6));
        assert_eq!(harness.loop_spec.terminate_on, ["END_SESSION", "CONSENSUS_REACHED"]);
        assert_eq!(
            harness.loop_spec.consensus_keyword(),
            Some("CONSENSUS_REACHED")
        );

        let field = |name: &str| {
            harness
                .result
                .iter()
                .find(|field| field.name == name)
                .expect("declared field")
                .result_type()
        };
        assert_eq!(field("consensus"), ResultType::Bool);
        assert_eq!(field("summary"), ResultType::Str);
    }

    #[test]
    fn discussion_has_no_consensus_terminator() {
        let harness = load_gameplan("discussion").expect("built-in loads").harness;
        assert_eq!(harness.loop_spec.terminate_on, ["END_SESSION"]);
        assert_eq!(harness.loop_spec.consensus_keyword(), None);
    }

    #[test]
    fn every_gameplan_has_think_and_rethink() {
        // The think/rethink principle is structural, not prose.
        for name in list_builtin_gameplans() {
            let phases = load_gameplan(&name)
                .expect("built-in loads")
                .harness
                .loop_spec
                .phases;
            assert!(!phases.is_empty(), "{name} declares no phases");
            assert_eq!(phases[0].name, "think");
            assert!(!phases[0].rethink);

            let rethinks: Vec<&_> = phases.iter().filter(|phase| phase.rethink).collect();
            assert_eq!(rethinks.len(), 1, "{name} must have exactly one rethink phase");
            assert_eq!(
                rethinks[0],
                phases.last().expect("non-empty"),
                "rethink must come last"
            );

            for phase in &phases {
                assert!(
                    !phase.instruction.is_empty(),
                    "{name}.{} has no instruction",
                    phase.name
                );
            }
        }
    }

    #[test]
    fn a_custom_gameplan_loads_from_an_absolute_path() {
        let file = TempFile::new(
            "custom",
            "---\nname: custom\nagents:\n  orchestrator: false\nloop:\n  max_rounds: 7\n---\n\n# Custom\n",
        );
        let config = load_gameplan(&file.name()).expect("custom loads");

        assert_eq!(config.name, "custom");
        assert!(!config.requires_orchestrator());
        assert_eq!(config.max_rounds(), 7);
        assert_eq!(config.body, "# Custom");
    }

    #[test]
    fn an_undeclared_name_falls_back_to_the_filename() {
        // A filename with underscores must not fail slug validation, because
        // the user did not choose it as an identifier.
        let file = TempFile::new("no_name_here", "---\nloop:\n  max_rounds: 2\n---\n\n# Relative\n");
        let config = load_gameplan(&file.name()).expect("custom loads");
        assert_eq!(config.name, "kerness-gameplan-no_name_here");
    }

    #[test]
    fn invalid_yaml_reports_the_file() {
        let file = TempFile::new("broken", "---\nname: [unclosed\n---\n\n# Bad\n");
        let error = load_gameplan(&file.name()).expect_err("broken frontmatter");
        assert!(error.to_string().contains("Invalid YAML"), "{error}");
    }

    #[test]
    fn a_file_with_no_frontmatter_loads_on_harness_defaults() {
        // The whole file becomes the instruction body, quietly. This is what
        // keeps a one-paragraph custom gameplan viable.
        let file = TempFile::new("plain", "# Plain\n\nJust instructions, no contract.\n");
        let config = load_gameplan(&file.name()).expect("plain markdown loads");

        assert_eq!(config.harness.loop_spec.terminate_on, ["END_SESSION"]);
        assert!(config.body.contains("Just instructions"));
    }
}
