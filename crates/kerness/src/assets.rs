//! Where the bundled gameplans, personas, and skills live.
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

fn slot() -> &'static RwLock<Option<PathBuf>> {
    static SLOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Resolve the bundled assets from *path* instead of the defaults.
pub fn set_root(path: impl Into<PathBuf>) {
    *slot().write().expect("assets lock poisoned") = Some(path.into());
}

/// The directory holding `gameplans/`, `personas/`, and `skills/`.
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
        .filter_map(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .collect();
    stems.sort();
    stems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_copy_is_found_without_any_wiring() {
        assert!(
            root().join("personas").join("pragmatic_engineer.md").exists(),
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
