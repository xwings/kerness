//! Where the bundled gameplans, roles, personas, and skills live, and the
//! reading every asset family shares.
//!
//! The assets are files on disk rather than strings compiled into the binary,
//! because a skill directory is also a *bundle*: the skill's own frontmatter
//! can grant read access to files beside its `SKILL.md`, and a path that only
//! exists inside the executable cannot be granted, listed, or read.
//!
//! Which directory that is depends on how the crate is being used, so the root
//! is resolved in three steps and the first answer wins:
//!
//! 1. [`set_root`] — what the Python bindings call at import, pointing at the
//!    installed package's own directory.
//! 2. `$KERNESS_ASSETS` — for an embedder that relocates the files.
//! 3. `$CARGO_MANIFEST_DIR/assets` — the copy in this repository, which is what
//!    a `cargo test` run and a Rust-only dependent see.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::pyfmt;
use crate::yaml;

fn slot() -> &'static RwLock<Option<PathBuf>> {
    static SLOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Resolve the bundled assets from *path* instead of the defaults.
pub fn set_root(path: impl Into<PathBuf>) {
    *slot().write().expect("assets lock poisoned") = Some(path.into());
}

/// The directory holding `gameplans/`, `roles/`, `personas/`, and `skills/`.
pub fn root() -> PathBuf {
    if let Some(path) = slot().read().expect("assets lock poisoned").clone() {
        return path;
    }
    if let Some(path) = std::env::var_os("KERNESS_ASSETS") {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Names of the `.md` files directly under *directory*, sorted.
///
/// Enumerated from disk rather than listed in code: a literal list goes stale
/// the moment an asset is added or removed, and nothing fails when it does. An
/// unreadable directory lists as empty, which is what a caller asking "what is
/// bundled?" needs to hear when the answer is "nothing".
pub fn list_markdown_stems(directory: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    stems.sort();
    stems
}

/// Every location an asset path could mean, in resolution order: the path as
/// written, then relative to each *search* directory, then relative to
/// *builtin*.
///
/// An absolute path means one place. Joining it onto a search directory already
/// discards that directory, so leaving this branch out would still resolve
/// correctly — it would just report the same path three times in the not-found
/// message, which reads as a bug in the search rather than a missing file.
pub fn candidates(path: &str, search: &[PathBuf], builtin: &Path) -> Vec<PathBuf> {
    let given = PathBuf::from(path);
    if given.is_absolute() {
        return vec![given];
    }
    let mut candidates = Vec::with_capacity(search.len() + 2);
    candidates.push(given);
    candidates.extend(search.iter().map(|directory| directory.join(path)));
    candidates.push(builtin.join(path));
    candidates
}

/// Resolve an asset path to a file that exists, or say where it was looked for.
///
/// *kind* is the capitalised family name the failure reports — "Role",
/// "Persona". The error names every directory tried, because "file not found"
/// without the search path sends the reader hunting for a resolution order they
/// cannot see.
///
/// Two families re-export a resolver of their own — [`crate::role`] and
/// [`crate::persona`] — and two keep theirs private. The rule is *search*: a
/// loader that takes a list of directories is one whose caller chose them, and
/// that caller needs to ask where a name would land before committing to it. A
/// gameplan and a skill resolve from fixed places, so there is nothing to ask.
pub fn resolve_path(kind: &str, path: &str, search: &[PathBuf], builtin: &Path) -> Result<PathBuf> {
    let mut tried: Vec<String> = Vec::new();
    for candidate in candidates(path, search, builtin) {
        if candidate.exists() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(Error::NotFound(format!(
        "{kind} file not found: {path}. Tried: {}.",
        tried.join(", ")
    )))
}

/// `str(meta.get(key, "")).strip()`, over a parsed frontmatter mapping.
pub fn text_field(meta: &Map<String, Value>, key: &str) -> String {
    meta.get(key)
        .map(pyfmt::str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Split an asset file into its frontmatter mapping and its body.
///
/// Line-based rather than the pattern [`crate::gameplan`] uses: the closing
/// delimiter is an exact `---` line, so a body containing `---` at the start of
/// a line with trailing spaces does not end the frontmatter early.
///
/// A file with no frontmatter at all is a body and nothing else — a participant
/// role written as one paragraph in a file, which is the smallest useful asset
/// there is and should not need three lines of ceremony to say so.
///
/// *kind* is the capitalised family name the failures report, as in
/// [`resolve_path`]; *source* is the file they name, because a mapping error
/// with no filename is unactionable in a session that loaded a dozen of these.
pub fn split_frontmatter(
    text: &str,
    kind: &str,
    source: &str,
) -> Result<(Map<String, Value>, String)> {
    if !text.starts_with("---") {
        return Ok((Map::new(), text.to_string()));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(offset) = lines
        .get(1..)
        .and_then(|rest| rest.iter().position(|line| *line == "---"))
    else {
        return Ok((Map::new(), text.to_string()));
    };
    let end = offset + 1;
    let body = lines[end + 1..]
        .join("\n")
        .trim_start_matches('\n')
        .to_string();

    match yaml::parse(&lines[1..end].join("\n")).map_err(|err| {
        Error::Value(format!(
            "Invalid YAML in {kind} frontmatter: {source}: {err}"
        ))
    })? {
        Value::Null => Ok((Map::new(), body)),
        Value::Object(map) => Ok((map, body)),
        _ => Err(Error::Value(format!(
            "{kind} frontmatter must be a mapping: {source}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_copy_is_found_without_any_wiring() {
        assert!(
            root()
                .join("personas")
                .join("pragmatic_engineer.md")
                .exists(),
            "cargo test resolves assets from the manifest directory"
        );
    }

    #[test]
    fn stems_are_listed_from_disk_and_sorted() {
        assert_eq!(
            list_markdown_stems(&root().join("gameplans")),
            vec!["debate", "discussion", "research"]
        );
    }

    #[test]
    fn a_directory_that_is_not_there_lists_as_empty() {
        assert!(list_markdown_stems(&root().join("nothing-here")).is_empty());
    }
}
