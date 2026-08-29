//! Role files: what an agent *is* in a session.
//!
//! A role answers *what is your position and job here* — the loop reads it to
//! decide who conducts and who contributes, and the base system prompt is the
//! role's own body. A [persona](crate::persona) answers *who are you*, and only
//! the prompt reads that. The two compose: an agent can be the orchestrator and
//! a devil's advocate at once.
//!
//! A role is a Markdown file with YAML frontmatter rather than the `##` section
//! layout a persona uses, because the one thing the framework must read out of
//! it — `position` — is structural. A heading a human is free to rename is the
//! wrong place to keep the value that decides who runs the session.

use std::path::PathBuf;

use crate::assets;
use crate::error::{Error, Result};
use crate::pyfmt;

/// Where an agent sits in the loop.
///
/// A closed set rather than a string: an unrecognised position satisfies
/// neither [`Agent::is_orchestrator`](crate::agent::Agent::is_orchestrator) nor
/// the orchestrator lookup, so accepting one would turn the session's conductor
/// into an extra contributor with no error anywhere.
///
/// [`Agent::role`](crate::agent::Agent::role) is the open half of the pair —
/// any built-in name, any `.md` file, any prose — and this is the only thing
/// the framework reads out of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Participant,
    Orchestrator,
}

impl Position {
    /// The name this position is written with in a role file.
    pub fn as_str(self) -> &'static str {
        match self {
            Position::Participant => "participant",
            Position::Orchestrator => "orchestrator",
        }
    }

    /// Read a position from its written name.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "participant" => Ok(Position::Participant),
            "orchestrator" => Ok(Position::Orchestrator),
            other => Err(Error::Value(format!(
                "Unknown agent position {}. Expected 'participant' or 'orchestrator'.",
                pyfmt::repr_str(other)
            ))),
        }
    }
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The built-in role an agent that names none of its own is given.
pub const DEFAULT_ROLE_FILE: &str = "participant.md";

/// A parsed role file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleConfig {
    /// The frontmatter `name`, or the file's stem when it has none.
    pub name: String,
    /// Where this role sits in the loop. Absent frontmatter means participant,
    /// which is the same answer an agent naming no role at all gets.
    pub position: Position,
    /// One line saying what the role is for. Not sent to any model; it is what
    /// `list_builtin_roles` callers and the self-check print.
    pub description: String,
    /// The body. For a participant this is the base system prompt; for an
    /// orchestrator it is the template
    /// [`Session::run`](crate::session::Session::run) fills from the harness
    /// contract.
    pub content: String,
}

/// The built-in role directory.
fn roles_dir() -> PathBuf {
    assets::root().join("roles")
}

/// Load a role from a `.md` file.
///
/// *search* is tried between the working directory and the built-ins, exactly
/// as [`load_persona`](crate::persona::load_persona) does it, so a third-party
/// project can ship a gameplan and its roles side by side.
pub fn load_role(path: &str, search: &[PathBuf]) -> Result<RoleConfig> {
    let resolved = resolve_role_path(path, search)?;
    let source = resolved.display().to_string();
    let text =
        std::fs::read_to_string(&resolved).map_err(|err| Error::Io(format!("{source}: {err}")))?;
    let (meta, body) = assets::split_frontmatter(&text, "Role", &source)?;

    let stem = resolved
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = assets::text_field(&meta, "name");
    let position = match meta.get("position") {
        Some(value) if !value.is_null() => Position::parse(pyfmt::str(value).trim())
            .map_err(|err| Error::Value(format!("{err} In {source}.")))?,
        _ => Position::Participant,
    };

    Ok(RoleConfig {
        name: if name.is_empty() { stem } else { name },
        position,
        description: assets::text_field(&meta, "description"),
        content: body.trim().to_string(),
    })
}

/// The names of all built-in roles.
pub fn list_builtin_roles() -> Vec<String> {
    assets::list_markdown_stems(&roles_dir())
}

/// The file a `role:` spec names, or `None` when the spec is prose.
///
/// A spec that looks like a path — it holds a separator or ends with `.md` —
/// always means a file, and not finding it is an error rather than a quiet
/// demotion to prose. `role = "./roles/typo.md"` becoming the literal role
/// description `./roles/typo.md` is the same silent-success failure
/// [`Agent::resolve_persona`](crate::agent::Agent) refuses for personas, and it
/// costs a whole run of provider calls to discover.
///
/// A bare name is a file only when one exists, so `"orchestrator"` finds the
/// built-in while `"a sceptical reviewer"` stays prose.
pub fn role_file(spec: &str, search: &[PathBuf]) -> Result<Option<PathBuf>> {
    if spec.ends_with(".md") || spec.contains('/') || spec.contains('\\') {
        return resolve_role_path(spec, search).map(Some);
    }
    let named = format!("{spec}.md");
    Ok(assets::candidates(&named, search, &roles_dir())
        .into_iter()
        .find(|candidate| candidate.is_file()))
}

/// Resolve a role path to a file that exists.
pub fn resolve_role_path(path: &str, search: &[PathBuf]) -> Result<PathBuf> {
    assets::resolve_path("Role", path, search, &roles_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn frontmatter_and_body_are_both_read() {
        let dir = TempDir::new("full");
        let path = dir.write(
            "judge.md",
            "---\nname: judge\nposition: orchestrator\ndescription: Runs the room.\n---\n\nYou judge.\n",
        );
        let role = load_role(&path.display().to_string(), &[]).expect("load");

        assert_eq!(role.name, "judge");
        assert_eq!(role.position, Position::Orchestrator);
        assert_eq!(role.description, "Runs the room.");
        assert_eq!(role.content, "You judge.");
    }

    #[test]
    fn a_file_with_no_frontmatter_is_a_participant_named_for_itself() {
        // The smallest useful role: one paragraph, no ceremony. Defaulting the
        // position the other way would hand the session's conductor's seat to
        // anyone who wrote a bare paragraph in a file.
        let dir = TempDir::new("bare");
        let path = dir.write("sceptic.md", "Doubt everything.\n");
        let role = load_role(&path.display().to_string(), &[]).expect("load");

        assert_eq!(role.name, "sceptic");
        assert_eq!(role.position, Position::Participant);
        assert_eq!(role.content, "Doubt everything.");
    }

    #[test]
    fn an_unknown_position_names_the_file_it_came_from() {
        let dir = TempDir::new("bad-position");
        let path = dir.write("odd.md", "---\nposition: referee\n---\n\nBody.\n");
        let error = load_role(&path.display().to_string(), &[]).expect_err("unknown position");

        assert!(
            error
                .to_string()
                .starts_with("Unknown agent position 'referee'."),
            "{error}"
        );
        assert!(error.to_string().contains("odd.md"), "{error}");
    }

    #[test]
    fn both_built_ins_load_by_bare_name() {
        assert_eq!(
            load_role("participant.md", &[]).expect("load").position,
            Position::Participant
        );
        assert_eq!(
            load_role("orchestrator.md", &[]).expect("load").position,
            Position::Orchestrator
        );
    }

    #[test]
    fn the_built_ins_are_enumerated_from_disk() {
        assert_eq!(list_builtin_roles(), vec!["orchestrator", "participant"]);
    }

    #[test]
    fn a_bare_name_finds_a_file_and_prose_does_not() {
        assert_eq!(
            role_file("orchestrator", &[]).expect("resolve"),
            Some(roles_dir().join("orchestrator.md"))
        );
        assert_eq!(
            role_file("a sceptical reviewer", &[]).expect("resolve"),
            None
        );
    }

    #[test]
    fn a_missing_path_is_an_error_rather_than_prose() {
        // The whole point of the distinction: a typo'd path must not become the
        // agent's role description and let the run look healthy.
        let dir = TempDir::new("missing");
        let error =
            role_file("roles/typo.md", std::slice::from_ref(&dir.0)).expect_err("no such file");
        let message = error.to_string();

        assert!(
            message.starts_with("Role file not found: roles/typo.md. Tried: "),
            "{message}"
        );
        assert!(message.contains(&dir.0.join("roles/typo.md").display().to_string()));
        assert!(message.contains(&roles_dir().join("roles/typo.md").display().to_string()));
    }

    #[test]
    fn a_search_directory_is_tried_before_the_built_ins() {
        let dir = TempDir::new("search");
        dir.write(
            "orchestrator.md",
            "---\nposition: participant\n---\n\nMine.\n",
        );
        assert_eq!(
            role_file("orchestrator", std::slice::from_ref(&dir.0)).expect("resolve"),
            Some(dir.0.join("orchestrator.md"))
        );
    }

    #[test]
    fn a_position_round_trips_through_its_written_name() {
        for position in [Position::Participant, Position::Orchestrator] {
            assert_eq!(Position::parse(position.as_str()).expect("parse"), position);
            assert_eq!(position.to_string(), position.as_str());
        }
    }

    #[test]
    fn a_position_that_is_nearly_right_is_still_rejected() {
        for written in ["orchestrater", "moderator", "Orchestrator"] {
            let error = Position::parse(written).expect_err("a typo is not a position");
            assert_eq!(
                error.to_string(),
                format!(
                    "Unknown agent position '{written}'. Expected 'participant' or \
                     'orchestrator'."
                )
            );
        }
    }
}
