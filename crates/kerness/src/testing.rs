//! A scratch directory for the crate's own unit tests.
//!
//! Compiled only under `cfg(test)` and never part of the public surface. It
//! exists because eleven modules each need "a directory that exists now and is
//! gone afterwards", and eleven hand-rolled copies of that drift: one
//! canonicalizes and one does not, one cleans up on panic and one does not, and
//! the difference only shows as a flake.
//!
//! The integration suite keeps its own copy in `tests/common/mod.rs`, because
//! that is a separate crate and cannot see anything behind `cfg(test)`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory removed when the handle drops.
pub struct TempDir(pub PathBuf);

/// Distinguishes two directories asked for under the same tag.
static NEXT: AtomicUsize = AtomicUsize::new(0);

impl TempDir {
    /// A fresh empty directory, named for *tag*.
    pub fn new(tag: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("kerness-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    /// The same, with symlinks resolved.
    ///
    /// The system temp directory is itself a symlink on macOS and on some Linux
    /// setups. A test that compares the path the access policy resolved, or the
    /// `pwd` a child process printed, needs the resolved form or it compares
    /// two spellings of one directory and fails.
    pub fn resolved(tag: &str) -> Self {
        let mut dir = TempDir::new(tag);
        dir.0 = std::fs::canonicalize(&dir.0).expect("canonicalize temp dir");
        dir
    }

    /// The directory as a string, for an API that takes one.
    pub fn text(&self) -> String {
        self.0.display().to_string()
    }

    /// A path inside the directory. The file need not exist.
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The same, as a string.
    pub fn child(&self, name: &str) -> String {
        self.join(name).display().to_string()
    }

    /// Write *text* to `name` inside the directory, and return its path.
    pub fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, text).expect("write file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
