//! What the agents in a session remember, and where it is kept.
//!
//! A session reads and writes its notes through a [`MemoryStore`]. The default
//! is [`FileMemory`], which is prose in a Markdown file the caller owns: the
//! framework imposes no heading, bullet, or template on it and never creates it
//! on load, because a session that only reads must leave nothing behind on disk
//! and a template written at load time would be both a trace and a format the
//! user did not choose. A caller who wants something else — a database, an
//! embedding index, a summarising store — installs it with
//! [`SessionConfig::memory_store`](crate::session::SessionConfig::memory_store)
//! and the session is unchanged.
//!
//! Three things follow from the notes being shared, and all three live here
//! rather than at the call sites that discovered them:
//!
//! - What one agent writes, every other agent reads *inside its system
//!   prompt*. That makes the store a channel between agents, so text arriving
//!   from a model is filtered on the way in — see [`MemoryFilter`] — and framed
//!   as quoted material on the way out, in [`crate::prompting::memory_block`].
//! - Notes outlive the run that wrote them. A session resumed a week later
//!   reads week-old notes with nothing to say they are old, so
//!   [`MemoryStore::age`] reports how stale a scope is and the prompt carries
//!   the caveat.
//! - A store is not trusted to be a file. [`MemoryStore::path`] is how one that
//!   *is* opts into the same workspace confinement the session file goes
//!   through; a store that answers `None` writes nothing the access boundary
//!   can confine, and the caller who installed it owns that decision.
//!
//! [`Memory`] is the file primitive underneath `FileMemory`, and remains
//! usable on its own by a caller who wants one file and no session.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// Where a session keeps what its agents remember.
///
/// The session holds one store and addresses it by *scope*: a name naming one
/// body of notes. The session's own scope is
/// [`SessionConfig::memory`](crate::session::SessionConfig::memory), and an
/// agent that declared [`Agent::memory`](crate::agent::Agent::memory) has that
/// string as its scope instead. What the string means is the store's to decide
/// — [`FileMemory`] reads it as a path, and a store backed by anything else
/// reads it as a key. The session never parses it.
///
/// Every method takes `&self`. One store serves every agent in a run and the
/// session holds it behind an `Arc`, so an implementation that caches or
/// batches keeps its own lock rather than borrowing the session's.
///
/// The four defaulted methods are the ones a store can honestly have no answer
/// for. Only [`read`](MemoryStore::read) and [`append`](MemoryStore::append)
/// must be written, because a store that cannot do both is not one.
pub trait MemoryStore: Send + Sync {
    /// Everything stored under *scope*, as the prompt should quote it.
    ///
    /// Called once per agent per turn, while the system prompt is assembled.
    fn read(&self, scope: &str) -> Result<String>;

    /// Store *note* under *scope*, as its own entry.
    ///
    /// The note arrives exactly as the writer wrote it, and has already passed
    /// whatever [`MemoryFilter`] the session installed: a store is never the
    /// place that filter is enforced, so a third-party one cannot skip it.
    fn append(&self, scope: &str, note: &str) -> Result<()>;

    /// Prepare *scope*, once per scope at the top of
    /// [`Session::run`](crate::session::Session::run).
    ///
    /// The point of it is *when* it happens: a store that cannot reach what it
    /// is backed by says so before the first provider call rather than in the
    /// middle of a turn. Defaulted to success because a store with nothing to
    /// open has nothing to fail at.
    fn open(&self, scope: &str) -> Result<()> {
        let _ = scope;
        Ok(())
    }

    /// Whole days since *scope* was last written, or `None` when nothing says.
    ///
    /// `None` is the answer a prompt wants when the age is unknown — it omits
    /// the staleness caveat rather than guessing at one — so a store with no
    /// notion of a write time takes the default and is correct.
    fn age(&self, scope: &str) -> Option<u64> {
        let _ = scope;
        None
    }

    /// The file this store writes for *scope*, for the workspace to confine.
    ///
    /// Defaulted to `None` for the same reason
    /// [`Channel::paths`](crate::channel::Channel::paths) is defaulted to
    /// nothing: most of the interesting stores write no file at all. A
    /// file-backed one overrides it and is confined exactly as the session file
    /// is.
    fn path(&self, scope: &str) -> Option<PathBuf> {
        let _ = scope;
        None
    }

    /// The run is over; settle whatever is outstanding.
    ///
    /// Called once at the end of `run()`, after the session result has been
    /// written. This is where a store that consolidates — summarising a run's
    /// notes, pruning, reindexing — does it, because it is the only moment at
    /// which the whole run is known and nothing further will be appended.
    /// [`FileMemory`] writes through on every append and so has nothing to do.
    fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// The default store: one Markdown file per scope, and the scope is the path.
///
/// Each scope's content is cached on first use and written through on every
/// append, so a reader never pays for a re-read and a crash mid-run still
/// leaves the file holding everything committed before it.
#[derive(Default)]
pub struct FileMemory {
    /// One [`Memory`] per scope, opened on demand.
    ///
    /// `Mutex` rather than `RwLock` because every operation here can write: a
    /// read of an unopened scope loads it, which is a write to this map.
    files: Mutex<HashMap<String, Memory>>,
}

impl FileMemory {
    /// A store with nothing open yet.
    pub fn new() -> Self {
        FileMemory::default()
    }

    /// Run *act* against *scope*'s file, loading it if this is its first use.
    ///
    /// Loading here rather than only in [`MemoryStore::open`] is what makes the
    /// store correct for a caller who never calls `open` — the session does,
    /// but the trait does not require it and a lazily-loaded scope reads the
    /// same either way.
    fn with<T>(&self, scope: &str, act: impl FnOnce(&mut Memory) -> Result<T>) -> Result<T> {
        let mut files = self.files.lock().unwrap_or_else(|err| err.into_inner());
        if !files.contains_key(scope) {
            let mut memory = Memory::new(scope);
            memory.load()?;
            files.insert(scope.to_string(), memory);
        }
        act(files.get_mut(scope).expect("inserted above"))
    }
}

impl MemoryStore for FileMemory {
    fn read(&self, scope: &str) -> Result<String> {
        self.with(scope, |memory| Ok(memory.read().to_string()))
    }

    fn append(&self, scope: &str, note: &str) -> Result<()> {
        self.with(scope, |memory| memory.append_entry(note))
    }

    fn open(&self, scope: &str) -> Result<()> {
        self.with(scope, |_| Ok(()))
    }

    /// Read from the file's mtime rather than from the cache.
    ///
    /// The cache says what the content is, not when it was written, and the
    /// module imposes no format on the content — a timestamp parsed out of
    /// prose would be a format.
    fn age(&self, scope: &str) -> Option<u64> {
        Memory::new(scope).age()
    }

    fn path(&self, scope: &str) -> Option<PathBuf> {
        Some(PathBuf::from(scope))
    }
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

    #[test]
    fn the_default_store_keeps_one_file_per_scope() {
        let dir = TempDir::new("scopes");
        let alice = dir.join("alice.md").display().to_string();
        let shared = dir.join("shared.md").display().to_string();
        let store = FileMemory::new();

        store.append(&alice, "mine").expect("append");
        store.append(&shared, "ours").expect("append");

        assert_eq!(store.read(&alice).expect("read"), "mine\n");
        assert_eq!(store.read(&shared).expect("read"), "ours\n");
        assert_eq!(
            fs::read_to_string(&alice).expect("read back"),
            "mine\n",
            "the scope is the path, so the file is where the caller named it"
        );
    }

    #[test]
    fn a_scope_never_opened_still_reads_what_is_on_disk() {
        let dir = TempDir::new("lazy");
        let path = dir.join("memory.md").display().to_string();
        Memory::new(&path)
            .append_entry("written earlier")
            .expect("append");

        let store = FileMemory::new();
        assert_eq!(store.read(&path).expect("read"), "written earlier\n");
    }

    #[test]
    fn the_default_store_reports_its_path_and_the_age_of_the_file() {
        let dir = TempDir::new("path-and-age");
        let path = dir.join("memory.md").display().to_string();
        let store = FileMemory::new();

        assert_eq!(store.path(&path), Some(PathBuf::from(&path)));
        assert_eq!(store.age(&path), None, "there is no file to date yet");
        store.append(&path, "now").expect("append");
        assert_eq!(store.age(&path), Some(0));
    }

    /// A store with nothing to open, date, or confine, exercising every default.
    #[derive(Default)]
    struct Ephemeral {
        notes: Mutex<Vec<String>>,
    }

    impl MemoryStore for Ephemeral {
        fn read(&self, _scope: &str) -> Result<String> {
            Ok(self.notes.lock().expect("lock").join("\n"))
        }

        fn append(&self, _scope: &str, note: &str) -> Result<()> {
            self.notes.lock().expect("lock").push(note.to_string());
            Ok(())
        }
    }

    #[test]
    fn a_store_writing_no_file_answers_the_defaults_and_leaves_no_trace() {
        let store = Ephemeral::default();
        store.append("anything", "kept in memory").expect("append");

        assert_eq!(store.read("anything").expect("read"), "kept in memory");
        assert_eq!(
            store.path("anything"),
            None,
            "nothing for a workspace to confine"
        );
        assert_eq!(store.age("anything"), None, "no write time, so no caveat");
        store.open("anything").expect("nothing to open");
        store.close().expect("nothing to settle");
    }
}
