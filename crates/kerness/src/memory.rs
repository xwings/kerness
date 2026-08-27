//! A Markdown file the agents in a session share.
//!
//! Memory is prose in a file the caller owns. The framework imposes no
//! heading, bullet, or template on it and never creates it on load: a session
//! that only reads must leave nothing behind on disk, and a template written
//! at load time would be both a trace and a format the user did not choose.
//!
//! The content is cached in memory and written through on every mutation, so a
//! reader never pays for a re-read and a crash mid-run still leaves the file
//! holding everything that was committed before it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// A memory file and its cached contents.
#[derive(Clone, Debug)]
pub struct Memory {
    path: PathBuf,
    content: String,
}

impl Default for Memory {
    fn default() -> Self {
        Memory::new("memory.md")
    }
}

impl Memory {
    /// Point at *path*, without touching the filesystem.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Memory {
            path: path.into(),
            content: String::new(),
        }
    }

    /// The configured path to the memory file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the file into the cache; an absent file reads as empty.
    ///
    /// A missing file is deliberately not created here — see the module
    /// documentation.
    pub fn load(&mut self) -> Result<()> {
        if !self.path.exists() {
            self.content.clear();
            return Ok(());
        }
        self.content = fs::read_to_string(&self.path)
            .map_err(|err| Error::Io(format!("{}: {err}", self.path.display())))?;
        Ok(())
    }

    /// The full memory content.
    pub fn read(&self) -> &str {
        &self.content
    }

    /// Append *text* verbatim and write through.
    pub fn append(&mut self, text: &str) -> Result<()> {
        self.content.push_str(text);
        self.flush()
    }

    /// Append *text* as its own entry, separated by a blank line.
    ///
    /// Stored verbatim: attribution, timestamps, and headings are the writer's
    /// business, not this method's. Blank text writes nothing at all, so a
    /// model that answers a memory tool with whitespace does not punch a hole
    /// in the file.
    pub fn append_entry(&mut self, text: &str) -> Result<()> {
        let entry = text.trim();
        if entry.is_empty() {
            return Ok(());
        }
        let base = self.content.trim_end_matches('\n');
        self.content = if base.is_empty() {
            format!("{entry}\n")
        } else {
            format!("{base}\n\n{entry}\n")
        };
        self.flush()
    }

    /// Replace the whole file with *content*.
    pub fn write(&mut self, content: &str) -> Result<()> {
        self.content = content.to_string();
        self.flush()
    }

    /// Write the cache to disk, creating the parent directory if needed.
    ///
    /// The directory is created here rather than in [`Memory::load`], which
    /// does not touch the filesystem at all.
    fn flush(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|err| Error::Io(format!("{}: {err}", parent.display())))?;
            }
        }
        fs::write(&self.path, &self.content)
            .map_err(|err| Error::Io(format!("{}: {err}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that removes itself, so these tests leave no trace either.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-memory-{tag}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loading_an_absent_file_reads_empty_and_creates_nothing() {
        let dir = TempDir::new("absent");
        let path = dir.join("memory.md");
        let mut memory = Memory::new(&path);
        memory.load().expect("load");

        assert_eq!(memory.read(), "");
        assert!(!path.exists(), "a read-only session leaves no trace");
    }

    #[test]
    fn entries_are_separated_by_a_blank_line_and_nothing_else_is_added() {
        let dir = TempDir::new("entries");
        let mut memory = Memory::new(dir.join("memory.md"));
        memory.append_entry("first").expect("append");
        memory.append_entry("  second  ").expect("append");

        assert_eq!(memory.read(), "first\n\nsecond\n");
        assert_eq!(
            fs::read_to_string(memory.path()).expect("read back"),
            "first\n\nsecond\n"
        );
    }

    #[test]
    fn a_blank_entry_writes_nothing() {
        let dir = TempDir::new("blank");
        let path = dir.join("memory.md");
        let mut memory = Memory::new(&path);
        memory.append_entry("   \n  ").expect("append");

        assert_eq!(memory.read(), "");
        assert!(!path.exists(), "nothing was written, so nothing was created");
    }

    #[test]
    fn writing_creates_the_parent_directory() {
        let dir = TempDir::new("nested");
        let path = dir.join("deep").join("down").join("memory.md");
        let mut memory = Memory::new(&path);
        memory.write("notes").expect("write");

        assert_eq!(fs::read_to_string(&path).expect("read back"), "notes");
    }

    #[test]
    fn a_reload_sees_what_was_written() {
        let dir = TempDir::new("roundtrip");
        let path = dir.join("memory.md");
        Memory::new(&path).append_entry("remembered").expect("append");

        let mut reopened = Memory::new(&path);
        reopened.load().expect("load");
        assert_eq!(reopened.read(), "remembered\n");
    }
}
