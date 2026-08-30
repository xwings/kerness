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
//!
//! Two things follow from the file being shared, and both live here rather
//! than at the call sites that discovered them:
//!
//! - What one agent writes, every other agent reads *inside its system
//!   prompt*. That makes the file a channel between agents, so text arriving
//!   from a model is filtered on the way in — see [`MemoryFilter`] — and framed
//!   as quoted material on the way out, in [`crate::prompting::memory_block`].
//! - A file outlives the run that wrote it. A session resumed a week later
//!   reads week-old notes with nothing to say they are old, so [`Memory::age`]
//!   reports the file's age from its mtime and the prompt carries the caveat.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::{Error, Result};

/// Seconds in a day, for [`Memory::age`].
const SECONDS_PER_DAY: u64 = 86_400;

/// A filter over text an *agent* writes to memory.
///
/// Installed with [`SessionConfig::memory_filter`](crate::session::SessionConfig::memory_filter)
/// and applied at the two points model output reaches the file: the
/// `write_memory` tool and the `@MEMORY:` marker pass. A caller writing through
/// [`Memory`] directly is not filtered, because the caller is not the untrusted
/// party.
///
/// The framework ships no implementation. What counts as a secret, and what a
/// session is willing to persist, are the caller's to define — a redactor
/// guessing at it here would be wrong in both directions, and wrong silently.
pub trait MemoryFilter: Send + Sync {
    /// The text to store, or `None` to drop the note entirely.
    ///
    /// *actor* is the agent that wrote it, so a filter can hold one agent to a
    /// stricter rule than another.
    fn filter(&self, note: &str, actor: &str) -> Option<String>;
}

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

    /// Whole days since the file was last written, or `None` when there is no
    /// file to date.
    ///
    /// Read from the filesystem rather than from the content, because the
    /// module imposes no format on the content and a timestamp parsed out of
    /// prose would be a format. A clock that has gone backwards since the write
    /// reads as `0` rather than as an error: staleness is advisory, and no
    /// caveat is the right answer when the age is not credible.
    pub fn age(&self) -> Option<u64> {
        let modified = fs::metadata(&self.path)
            .and_then(|data| data.modified())
            .ok()?;
        let elapsed = SystemTime::now()
            .duration_since(modified)
            .map_or(0, |since| since.as_secs());
        Some(elapsed / SECONDS_PER_DAY)
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
    use crate::testing::TempDir;

    /// A directory that removes itself, so these tests leave no trace either.
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
        assert!(
            !path.exists(),
            "nothing was written, so nothing was created"
        );
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
    fn a_file_written_now_is_zero_days_old_and_an_absent_one_has_no_age() {
        let dir = TempDir::new("age");
        let path = dir.join("memory.md");
        let mut memory = Memory::new(&path);

        assert_eq!(memory.age(), None, "there is no file to date yet");
        memory.append_entry("just written").expect("append");
        assert_eq!(memory.age(), Some(0));
    }

    #[test]
    fn a_reload_sees_what_was_written() {
        let dir = TempDir::new("roundtrip");
        let path = dir.join("memory.md");
        Memory::new(&path)
            .append_entry("remembered")
            .expect("append");

        let mut reopened = Memory::new(&path);
        reopened.load().expect("load");
        assert_eq!(reopened.read(), "remembered\n");
    }
}
