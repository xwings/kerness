//! Skill file loading.
//!
//! A skill is a Markdown file whose YAML frontmatter names it and describes it
//! in one line, and whose body is the instruction set an agent only reads after
//! it decides it needs one. Built-in names and custom paths resolve the same
//! way gameplans do.
//!
//! The frontmatter goes through the YAML parser rather than a split on the
//! first `:`, because `allowed-tools` and `requires-tools` are lists and a
//! hand-rolled splitter cannot represent one.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::assets;
use crate::error::{Error, Result};
use crate::pyfmt;

/// Directories a skill may bundle alongside its `SKILL.md`.
pub const BUNDLE_DIRS: [&str; 2] = ["scripts", "references"];

/// A parsed skill.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillConfig {
    /// Slug, matching the parent directory when the file is a `SKILL.md`.
    pub name: String,
    /// The one line an agent sees before deciding to load this.
    pub description: String,
    /// The body, delivered only when the skill is invoked.
    pub content: String,
    /// Tools this skill permits while active. `None` means it does not narrow
    /// the agent's toolkit; an empty list narrows it to nothing. Restrictive
    /// only — see [`crate::skill::runtime`].
    pub allowed_tools: Option<Vec<String>>,
    /// Tools this skill cannot work without. Additive: loading the skill makes
    /// each of these callable out of what the session registered, past a
    /// gameplan's `tools:` list and past another skill's `allowed-tools`. A
    /// name nobody registered is a session error before the first turn.
    ///
    /// A plain list rather than an `Option`, because absent and empty say the
    /// same thing here — a skill that requires nothing.
    pub requires_tools: Vec<String>,
    /// The skill's own directory, used to resolve bundled files.
    pub base_dir: Option<PathBuf>,
    /// Whether the skill ships inside the package. Bundled-file access is
    /// granted automatically for built-ins and not for skills loaded from
    /// arbitrary paths.
    pub builtin: bool,
}

impl SkillConfig {
    /// The bundled directories this skill actually ships.
    pub fn bundle_paths(&self) -> Vec<PathBuf> {
        let Some(base) = &self.base_dir else {
            return Vec::new();
        };
        BUNDLE_DIRS
            .iter()
            .map(|name| base.join(name))
            .filter(|path| path.is_dir())
            .collect()
    }
}

/// The built-in skill directory.
fn skills_dir() -> PathBuf {
    assets::root().join("skills")
}

/// Load a skill from a built-in name or a file path.
///
/// If *name_or_path* looks like a path — it contains a separator or ends with
/// `.md` — it is resolved as-is first, then relative to the built-in `skills/`
/// directory. Otherwise it names a built-in skill.
pub fn load_skill(name_or_path: &str) -> Result<SkillConfig> {
    let path = resolve_skill_path(name_or_path)?;
    let source = path.display().to_string();

    let text =
        std::fs::read_to_string(&path).map_err(|err| Error::Io(format!("{source}: {err}")))?;
    let (meta, body) = assets::split_frontmatter(&text, "Skill", &source)?;

    let name = assets::text_field(&meta, "name");
    let description = assets::text_field(&meta, "description");
    if name.is_empty() || description.is_empty() {
        return Err(Error::Value(format!(
            "Skill file missing required frontmatter: {source}"
        )));
    }
    validate_skill_name(&name, &path, &source)?;
    validate_description(&description, &source)?;

    // Only a `SKILL.md` owns its directory. A loose `notes.md` sitting in
    // someone's home directory must not turn that directory into a bundle.
    let base_dir = is_skill_md(&path).then(|| path.parent().unwrap_or(Path::new("")).to_path_buf());

    Ok(SkillConfig {
        name,
        description,
        content: body.trim().to_string(),
        allowed_tools: parse_tool_list(meta.get("allowed-tools"), "allowed-tools", &source)?,
        requires_tools: parse_tool_list(meta.get("requires-tools"), "requires-tools", &source)?
            .unwrap_or_default(),
        base_dir,
        builtin: is_builtin(&path),
    })
}

/// The names of all built-in skills.
pub fn list_builtin_skills() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(skills_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").exists())
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
}

/// Whether *path* lives inside the package's own skills directory.
///
/// The flag decides whether activating the skill grants read access to the
/// files beside it, so it is answered about the resolved path: a symlink into
/// `skills/` from anywhere on disk would otherwise widen the grant to whatever
/// it points at.
fn is_builtin(path: &Path) -> bool {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = skills_dir();
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    resolved.starts_with(root)
}

/// Whether the file is a `SKILL.md`, under any casing.
fn is_skill_md(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().to_lowercase() == "skill.md")
}

/// Normalize a frontmatter key holding tool names, written either as a YAML
/// list or as one comma-separated string.
///
/// `None` is the key being absent, which the two callers read differently:
/// `allowed-tools` does not narrow, `requires-tools` requires nothing. An
/// explicit empty list is a real answer for `allowed-tools` — "no tools", which
/// a pure-instruction skill may legitimately declare.
fn parse_tool_list(value: Option<&Value>, key: &str, source: &str) -> Result<Option<Vec<String>>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if let Value::String(text) = value {
        return Ok(Some(
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
        ));
    }
    let names = value
        .as_array()
        .filter(|items| items.iter().all(Value::is_string))
        .ok_or_else(|| {
            Error::Value(format!(
                "{key} must be a list of tool names in {source}, got {}",
                pyfmt::repr(value)
            ))
        })?;
    Ok(Some(
        names
            .iter()
            .map(|name| pyfmt::str(name).trim().to_string())
            .collect(),
    ))
}

/// Resolve a skill name or path to an existing file.
fn resolve_skill_path(name_or_path: &str) -> Result<PathBuf> {
    let is_path =
        name_or_path.contains('/') || name_or_path.contains('\\') || name_or_path.ends_with(".md");

    if is_path {
        if let Some(found) = resolve_skill_candidate(Path::new(name_or_path)) {
            return Ok(found);
        }
        if let Some(found) = resolve_skill_candidate(&skills_dir().join(name_or_path)) {
            return Ok(found);
        }
        return Err(Error::NotFound(format!(
            "Skill file not found: {name_or_path}"
        )));
    }

    let directory = skills_dir().join(name_or_path);
    resolve_skill_candidate(&directory)
        .ok_or_else(|| Error::NotFound(format!("Skill file not found: {}", directory.display())))
}

/// A directory holding a `SKILL.md` means that file; a file means itself.
fn resolve_skill_candidate(candidate: &Path) -> Option<PathBuf> {
    if candidate.is_dir() {
        let skill_md = candidate.join("SKILL.md");
        if skill_md.exists() {
            return Some(skill_md);
        }
    }
    candidate.is_file().then(|| candidate.to_path_buf())
}

fn validate_skill_name(name: &str, path: &Path, source: &str) -> Result<()> {
    static NAME_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("static pattern"));

    if name.chars().count() > 64 || !NAME_RE.is_match(name) {
        return Err(Error::Value(format!(
            "Invalid skill name '{name}' in {source}"
        )));
    }
    if is_skill_md(path) {
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !parent.is_empty() && parent != name {
            return Err(Error::Value(format!(
                "Skill name '{name}' must match parent directory '{parent}'"
            )));
        }
    }
    Ok(())
}

fn validate_description(description: &str, source: &str) -> Result<()> {
    if description.is_empty() || description.chars().count() > 1024 {
        return Err(Error::Value(format!(
            "Invalid skill description in {source}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    /// Write a `<name>/SKILL.md` under *dir*, carrying *frontmatter* between the
    /// required `name` and `description` lines, and load it.
    fn skill(dir: &TempDir, name: &str, frontmatter: &str) -> Result<SkillConfig> {
        let path = dir.write(
            &format!("{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: A demo skill.\n{frontmatter}---\n\nBody.\n"),
        );
        load_skill(&path.display().to_string())
    }

    #[test]
    fn a_builtin_loads_by_bare_name_or_by_file() {
        let skill = load_skill("summarize").expect("built-in loads");
        assert_eq!(skill.name, "summarize");
        assert!(!skill.description.is_empty());
        assert_eq!(
            load_skill("summarize/SKILL.md")
                .expect("built-in loads")
                .name,
            skill.name
        );
    }

    #[test]
    fn a_missing_skill_is_a_file_not_found_either_way() {
        for missing in ["nonexistent_skill", "/nonexistent/custom_skill.md"] {
            let error = load_skill(missing).expect_err("no such skill");
            assert!(matches!(error, Error::NotFound(_)), "{error:?}");
            assert!(error.to_string().contains("not found"), "{error}");
        }
    }

    #[test]
    fn a_custom_skill_loads_from_a_path() {
        let dir = TempDir::new("custom");
        let skill = skill(&dir, "custom-skill", "").expect("custom loads");
        assert_eq!(skill.name, "custom-skill");
        assert_eq!(skill.description, "A demo skill.");
        assert_eq!(skill.content, "Body.");
    }

    #[test]
    fn the_builtins_are_enumerated_from_disk_and_sorted() {
        let skills = list_builtin_skills();
        for expected in ["summarize", "fact-check", "challenge"] {
            assert!(skills.iter().any(|name| name == expected), "{skills:?}");
        }
        let mut sorted = skills.clone();
        sorted.sort();
        assert_eq!(skills, sorted);
    }

    #[test]
    fn absent_narrows_nothing_and_an_empty_list_narrows_to_nothing() {
        // Collapsing the two would silently grant every tool to a skill that
        // declared it wanted none.
        let dir = TempDir::new("narrowing");
        assert_eq!(skill(&dir, "a", "").expect("loads").allowed_tools, None);
        assert_eq!(
            skill(&dir, "b", "allowed-tools: []\n")
                .expect("loads")
                .allowed_tools,
            Some(Vec::new())
        );
    }

    #[test]
    fn inline_and_block_lists_both_parse() {
        // Why the frontmatter goes through the YAML parser: a parser that split
        // on the first ':' could not represent either form.
        let dir = TempDir::new("lists");
        let expected = Some(vec!["read_file".to_string(), "cmd".to_string()]);
        assert_eq!(
            skill(&dir, "a", "allowed-tools: [read_file, cmd]\n")
                .expect("loads")
                .allowed_tools,
            expected
        );
        assert_eq!(
            skill(&dir, "b", "allowed-tools:\n  - read_file\n  - cmd\n")
                .expect("loads")
                .allowed_tools,
            expected
        );
    }

    #[test]
    fn a_comma_separated_string_is_split() {
        let dir = TempDir::new("commas");
        assert_eq!(
            skill(&dir, "a", "allowed-tools: read_file, cmd\n")
                .expect("loads")
                .allowed_tools,
            Some(vec!["read_file".to_string(), "cmd".to_string()])
        );
    }

    #[test]
    fn requires_tools_reads_the_same_shapes_and_defaults_to_none_required() {
        // The two keys share a parser, so this asserts the reading of the
        // second one rather than every shape a second time.
        let dir = TempDir::new("requires");
        assert_eq!(
            skill(&dir, "a", "").expect("loads").requires_tools,
            Vec::<String>::new()
        );
        assert_eq!(
            skill(&dir, "b", "requires-tools: [cmd]\n")
                .expect("loads")
                .requires_tools,
            vec!["cmd".to_string()]
        );
        let error = skill(&dir, "c", "requires-tools: {a: b}\n").expect_err("not a list");
        assert!(
            error.to_string().contains("requires-tools must be a list"),
            "{error}"
        );
    }

    #[test]
    fn a_non_list_and_malformed_yaml_are_both_reported() {
        let dir = TempDir::new("bad-tools");
        let error = skill(&dir, "a", "allowed-tools: {a: b}\n").expect_err("not a list");
        assert!(matches!(error, Error::Value(_)), "{error:?}");
        assert!(
            error.to_string().contains("allowed-tools must be a list"),
            "{error}"
        );
        assert!(error.to_string().ends_with("got {'a': 'b'}"), "{error}");

        let error = skill(&dir, "b", "allowed-tools: [unclosed\n").expect_err("broken yaml");
        assert!(error.to_string().contains("Invalid YAML"), "{error}");
    }

    #[test]
    fn both_name_and_description_are_required() {
        let dir = TempDir::new("frontmatter");
        let base = dir.0.join("demo");
        std::fs::create_dir_all(&base).expect("create skill dir");
        let path = base.join("SKILL.md");
        std::fs::write(&path, "---\nname: demo\n---\n\nBody.\n").expect("write skill");

        let error = load_skill(&path.display().to_string()).expect_err("no description");
        assert!(
            error
                .to_string()
                .starts_with("Skill file missing required frontmatter: "),
            "{error}"
        );
    }

    #[test]
    fn a_name_must_be_a_slug_and_must_match_its_directory() {
        let dir = TempDir::new("names");
        let base = dir.0.join("demo");
        std::fs::create_dir_all(&base).expect("create skill dir");
        let path = base.join("SKILL.md");
        let write = |name: &str| {
            std::fs::write(
                &path,
                format!("---\nname: {name}\ndescription: A demo.\n---\n"),
            )
            .expect("write skill");
            load_skill(&path.display().to_string())
        };

        assert!(write("Demo")
            .expect_err("not a slug")
            .to_string()
            .starts_with("Invalid skill name 'Demo' in "));
        assert_eq!(
            write("other").expect_err("wrong directory").to_string(),
            "Skill name 'other' must match parent directory 'demo'"
        );
        assert_eq!(write("demo").expect("matches").name, "demo");
    }

    #[test]
    fn only_builtin_skills_are_marked_builtin() {
        // The flag is what grants bundle access, so a path-loaded skill
        // claiming it would widen the grant to any directory on disk.
        assert!(load_skill("summarize").expect("built-in loads").builtin);

        let dir = TempDir::new("builtin");
        assert!(!skill(&dir, "demo", "").expect("loads").builtin);
    }

    #[test]
    fn only_existing_bundle_dirs_are_reported() {
        let dir = TempDir::new("bundles");
        std::fs::create_dir_all(dir.0.join("demo").join("references")).expect("create bundle");
        let skill = skill(&dir, "demo", "").expect("loads");

        let names: Vec<String> = skill
            .bundle_paths()
            .iter()
            .map(|path| {
                path.file_name()
                    .expect("named")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["references"]);
    }

    #[test]
    fn a_pathless_skill_has_no_bundles() {
        assert!(SkillConfig::default().bundle_paths().is_empty());
    }
}
