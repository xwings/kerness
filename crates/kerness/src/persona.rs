//! Persona files: parsing, resolution, and the prompt block they become.
//!
//! A persona is a Markdown file with three optional `##` sections. Markdown
//! rather than YAML because a persona is prose a human writes and reads, and
//! the only structure the framework needs from it is which paragraph is which.

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use crate::assets;
use crate::error::{Error, Result};

/// A parsed persona.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonaConfig {
    /// The `# Persona:` title, or the file's stem when it has none.
    pub name: String,
    pub persona: String,
    pub background: String,
    pub communication_style: String,
}

/// The built-in persona directory.
fn personas_dir() -> PathBuf {
    assets::root().join("personas")
}

/// Load a persona from a `.md` file.
///
/// *search* is tried between the working directory and the built-ins. The
/// session passes the loaded gameplan's own directory, so a third-party project
/// can ship a gameplan and its personas side by side and have the paths in that
/// gameplan resolve no matter where the process was started.
pub fn load_persona(path: &str, search: &[PathBuf]) -> Result<PersonaConfig> {
    let resolved = resolve_persona_path(path, search)?;
    let text = std::fs::read_to_string(&resolved)
        .map_err(|err| Error::Io(format!("{}: {err}", resolved.display())))?;

    static TITLE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)\A# Persona:\s*(.+)$").expect("static pattern"));

    let stem = resolved
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(PersonaConfig {
        name: TITLE_RE
            .captures(&text)
            .map(|captures| captures[1].trim().to_string())
            .unwrap_or(stem),
        persona: extract_section(&text, "Persona"),
        background: extract_section(&text, "Background"),
        communication_style: extract_section(&text, "Communication Style"),
    })
}

/// Format a persona for injection into a system prompt.
///
/// Absent sections contribute no line at all, so a persona that only sets a
/// communication style does not spend prompt on two empty labels.
pub fn format_persona_for_prompt(config: &PersonaConfig) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(3);
    if !config.persona.is_empty() {
        lines.push(format!("Persona: {}", config.persona));
    }
    if !config.background.is_empty() {
        lines.push(format!("Background: {}", config.background));
    }
    if !config.communication_style.is_empty() {
        lines.push(format!("Communication style: {}", config.communication_style));
    }
    lines.join("\n")
}

/// The names of all built-in personas.
pub fn list_builtin_personas() -> Vec<String> {
    assets::list_markdown_stems(&personas_dir())
}

/// Resolve a persona path to a file that exists.
///
/// Tried in order: the path as written, then relative to each *search*
/// directory, then relative to the built-ins. The error names every directory
/// tried, because "persona file not found" without the search path sends the
/// reader hunting for a resolution order they cannot see.
pub fn resolve_persona_path(path: &str, search: &[PathBuf]) -> Result<PathBuf> {
    let mut tried: Vec<String> = Vec::new();
    for candidate in candidates(path, search) {
        if candidate.exists() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(Error::NotFound(format!(
        "Persona file not found: {path}. Tried: {}.",
        tried.join(", ")
    )))
}

/// Every location a persona path could mean, in resolution order.
///
/// An absolute path means one place. Joining it onto a search directory
/// already discards that directory, so leaving this branch out would still
/// resolve correctly — it would just report the same path three times in the
/// not-found message, which reads as a bug in the search rather than a missing
/// file.
fn candidates(path: &str, search: &[PathBuf]) -> Vec<PathBuf> {
    let given = PathBuf::from(path);
    if given.is_absolute() {
        return vec![given];
    }
    let mut candidates = Vec::with_capacity(search.len() + 2);
    candidates.push(given);
    candidates.extend(search.iter().map(|directory| directory.join(path)));
    candidates.push(personas_dir().join(path));
    candidates
}

/// The content under a `## <heading>` heading, up to the next `## ` or the end.
///
/// A line scan rather than a pattern: the section ends at a lookahead the
/// `regex` crate cannot express, and the scan is both exact and cheaper than
/// the backtracking alternative.
fn extract_section(text: &str, heading: &str) -> String {
    let marker = format!("## {heading}");
    let mut lines = text.lines();
    let found = lines.by_ref().any(|line| {
        line.strip_prefix(&marker)
            .is_some_and(|rest| rest.chars().all(char::is_whitespace))
    });
    if !found {
        return String::new();
    }
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    body.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "# Persona: Dr. Ada\n",
        "\n",
        "## Persona\n",
        "A systems researcher.\n",
        "\n",
        "## Background\n",
        "Twenty years on distributed storage.\n",
        "\n",
        "## Communication Style\n",
        "Direct, and short.\n",
    );

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-persona-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).expect("write persona");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_section_is_parsed_and_the_title_wins_over_the_stem() {
        let dir = TempDir::new("sections");
        let path = dir.write("ada.md", SAMPLE);
        let config = load_persona(&path.display().to_string(), &[]).expect("load");

        assert_eq!(config.name, "Dr. Ada");
        assert_eq!(config.persona, "A systems researcher.");
        assert_eq!(config.background, "Twenty years on distributed storage.");
        assert_eq!(config.communication_style, "Direct, and short.");
    }

    #[test]
    fn a_file_with_no_title_is_named_for_itself() {
        let dir = TempDir::new("untitled");
        let path = dir.write("bob.md", "## Persona\nTerse.\n");
        let config = load_persona(&path.display().to_string(), &[]).expect("load");

        assert_eq!(config.name, "bob");
        assert_eq!(config.background, "", "an absent section is empty");
    }

    #[test]
    fn only_the_sections_that_are_set_reach_the_prompt() {
        let config = PersonaConfig {
            name: "Bob".into(),
            communication_style: "Terse.".into(),
            ..PersonaConfig::default()
        };
        assert_eq!(format_persona_for_prompt(&config), "Communication style: Terse.");
    }

    #[test]
    fn a_bare_name_resolves_to_the_built_ins() {
        let resolved = resolve_persona_path("pragmatic_engineer.md", &[]).expect("resolve");
        assert_eq!(resolved, personas_dir().join("pragmatic_engineer.md"));
    }

    #[test]
    fn a_search_directory_is_tried_before_the_built_ins() {
        let dir = TempDir::new("search");
        dir.write("pragmatic_engineer.md", SAMPLE);
        let resolved = resolve_persona_path("pragmatic_engineer.md", std::slice::from_ref(&dir.0))
            .expect("resolve");
        assert_eq!(resolved, dir.0.join("pragmatic_engineer.md"));
    }

    #[test]
    fn the_not_found_error_names_every_directory_tried() {
        let dir = TempDir::new("missing");
        let error =
            resolve_persona_path("nowhere.md", std::slice::from_ref(&dir.0)).expect_err("no such file");
        let message = error.to_string();

        assert!(message.starts_with("Persona file not found: nowhere.md. Tried: "));
        assert!(message.contains(&dir.0.join("nowhere.md").display().to_string()));
        assert!(message.contains(&personas_dir().join("nowhere.md").display().to_string()));
    }

    #[test]
    fn the_built_ins_are_enumerated_from_disk() {
        assert_eq!(
            list_builtin_personas(),
            vec!["devils_advocate", "pragmatic_engineer"]
        );
    }
}
