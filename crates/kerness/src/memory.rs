//! What the agents in a session remember, and where it is kept.
//!
//! A session reads and writes its notes through a [`MemoryStore`]. The default
//! is [`FileMemory`], which is prose in a Markdown file the caller owns: the
//! framework imposes no heading, bullet, or template on it and never creates it
//! on load, because a session that only reads must leave nothing behind on disk
//! and a template written at load time would be both a trace and a format the
//! user did not choose. A caller who wants something else — a database, an
//! embedding index — installs it with
//! [`SessionConfig::memory_store`](crate::session::SessionConfig::memory_store)
//! and the session is unchanged.
//!
//! The other two stores the crate ships are the reason the slot exists at all,
//! and they answer the same question differently: notes that only ever grow
//! eventually cost more of a prompt than they are worth, so something has to
//! bound them, and *who* does the bounding is the choice.
//!
//! - [`SummarizingMemory`] bounds a scope by entry count and spends one
//!   provider call at the end of the run folding the overflow into a running
//!   summary. The framework decides, and the agents never see it happen.
//! - [`CuratedMemory`] bounds a scope by characters and refuses the append that
//!   would cross the ceiling, telling the agent what is stored and to merge or
//!   remove something first. The agents decide, in the turn they were already
//!   taking, and no extra provider call is made.
//!
//! Neither is the default, because a caller who wants their notes kept exactly
//! as written should not have to opt out of having them rewritten or refused.
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
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::logging;
use crate::provider::{Provider, ReasoningEffort};

/// Seconds in a day, for [`days_since_write`].
const SECONDS_PER_DAY: u64 = 86_400;

/// Whole days since *path* was last written, or `None` when there is no file.
///
/// Read from the filesystem rather than from the content, because neither
/// bundled store imposes a format the content must carry a timestamp in, and a
/// timestamp parsed out of prose would be such a format. A clock that has gone
/// backwards since the write reads as `0` rather than as an error: staleness is
/// advisory, and no caveat is the right answer when the age is not credible.
fn days_since_write(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).and_then(|data| data.modified()).ok()?;
    let elapsed = SystemTime::now()
        .duration_since(modified)
        .map_or(0, |since| since.as_secs());
    Some(elapsed / SECONDS_PER_DAY)
}

/// The file *scope* is kept in, under *root*, named `<encoded>.<extension>`.
///
/// A scope here is a key, not a path. Every byte outside `[A-Za-z0-9_-]` is
/// written as `%XX`, which is reversible — so two scopes never collide on one
/// file — and leaves no separator and no `.` in the name, so a scope reading
/// like `../../elsewhere` addresses a file under the root with that name rather
/// than a file outside it.
///
/// Shared by the two stores that keep a root and take the scope as a key.
/// [`FileMemory`] does not use it: there the scope *is* the path, which is the
/// whole of what makes it the default.
fn scope_file(root: &Path, scope: &str, extension: &str) -> PathBuf {
    let mut name = String::with_capacity(scope.len());
    for byte in scope.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => name.push(byte as char),
            _ => name.push_str(&format!("%{byte:02X}")),
        }
    }
    root.join(format!("{name}.{extension}"))
}

/// Write *content* to *path*, creating the directory holding it if needed.
///
/// The bundled stores defer the `mkdir` to the first real write rather than
/// doing it when a scope is opened, so a run that reads memory and writes
/// nothing leaves no directory behind.
fn write_creating_parent(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::Io(format!("{}: {err}", parent.display())))?;
        }
    }
    fs::write(path, content).map_err(|err| Error::Io(format!("{}: {err}", path.display())))
}

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

    /// The character ceiling one scope may hold, or `None` for no ceiling.
    ///
    /// Answering is what makes a store *curated*: the ceiling is what lets
    /// [`append`](MemoryStore::append) refuse a note, and
    /// [`revise`](MemoryStore::revise) is what an agent gets back under it
    /// with. The session registers the `edit_memory` tool exactly when this is
    /// `Some`, and states the figure in the tool's description, so the two
    /// halves cannot be offered apart: a store with no ceiling has nothing to
    /// curate towards, and a session that could not revise would be told to get
    /// under a ceiling it has no means to reach.
    ///
    /// `None` is not "unlimited storage" — it is "this store does not enforce
    /// one", which is the honest answer for [`FileMemory`], whose file is the
    /// caller's, and for [`SummarizingMemory`], which bounds itself by count
    /// rather than by characters and does the bounding itself.
    fn budget(&self) -> Option<usize> {
        None
    }

    /// Replace the one entry under *scope* containing *old*, or remove it when
    /// *new* is empty.
    ///
    /// *old* addresses an entry by being a substring of exactly one of them:
    /// matching none or several is an error naming which, because a model that
    /// guessed at a fragment should be told to lengthen it rather than have the
    /// wrong entry silently rewritten. *new* replaces the whole entry, not the
    /// matched fragment, and has passed the session's [`MemoryFilter`] exactly
    /// as an appended note does.
    ///
    /// Defaulted to a refusal rather than to success: a store that keeps notes
    /// append-only has no entry to address, and reporting a revision it did not
    /// make would be the one failure its caller cannot detect. No agent meets
    /// this default through the `edit_memory` tool, because that tool is
    /// registered only where [`budget`](MemoryStore::budget) answered — so
    /// reaching it means either a direct call or a store that declared a
    /// ceiling it has no means to get back under.
    fn revise(&self, scope: &str, old: &str, new: &str) -> Result<()> {
        let _ = (scope, old, new);
        Err(Error::Value(REVISE_UNSUPPORTED.to_string()))
    }
}

/// What a store with nothing to revise answers.
///
/// A constant rather than a literal because the Python base class raises the
/// same refusal for a store subclassed there, and a message spelled out in both
/// languages drifts silently — `bindings/python/kerness/memory.py` imports this
/// one.
pub const REVISE_UNSUPPORTED: &str =
    "this session's memory keeps notes as they were written; an entry cannot \
     be revised or removed";

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
    /// file to date. The mtime is the source, not the content: an agent that
    /// writes nothing this run leaves a file that is genuinely a day older.
    pub fn age(&self) -> Option<u64> {
        days_since_write(&self.path)
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
        write_creating_parent(&self.path, &self.content)
    }
}

/// How many entries a scope keeps verbatim before the rest are consolidated.
pub const DEFAULT_KEEP_ENTRIES: usize = 20;

/// Prefix on the consolidated block inside what a scope reads as.
///
/// The counterpart of [`SUMMARY_PREFIX`](crate::compaction::SUMMARY_PREFIX),
/// and labelled for the same reason: an agent quoting its own memory should be
/// able to tell a framework-written recap from a note somebody wrote.
pub const CONSOLIDATED_PREFIX: &str = "Consolidated from earlier notes:";

/// Instruction handed to the model that consolidates a scope.
pub const CONSOLIDATE_PROMPT: &str = concat!(
    "The notes below are one body of a session's memory, and they have grown ",
    "past what is worth carrying in a prompt. Rewrite them as a single compact ",
    "set of standing facts: keep what a later session would need in order to ",
    "act correctly — decisions taken, constraints, names, figures, and what is ",
    "still open — and drop what only described the moment it was written. ",
    "Attribute a fact to whoever established it. Write only the notes."
);

/// A store that keeps recent notes verbatim and summarises the rest.
///
/// One JSON file per scope under *root*, holding a running summary and the
/// entries written since it was last rewritten. [`read`](MemoryStore::read)
/// renders the summary and then the entries;
/// [`append`](MemoryStore::append) writes through on every note, so a crash
/// mid-run loses nothing that was committed; and [`close`](MemoryStore::close)
/// — once, at the end of the run — folds everything past the most recent
/// [`keep`](SummarizingMemory::with_keep) entries into the summary with one
/// provider call per scope that overflowed.
///
/// The end of the run is the only honest moment for that call. It is the first
/// point at which the whole run is known and the last at which nothing further
/// will be appended, and doing it mid-turn would charge an agent's own turn for
/// rewriting notes it is about to read.
pub struct SummarizingMemory {
    root: PathBuf,
    provider: Arc<dyn Provider>,
    model: String,
    keep: usize,
    /// One [`Scope`] per scope, loaded on demand. `Mutex` rather than `RwLock`
    /// for [`FileMemory`]'s reason: a read of an unloaded scope loads it.
    scopes: Mutex<HashMap<String, Scope>>,
}

/// One scope's file, as it is held in memory.
#[derive(Default)]
struct Scope {
    /// Everything consolidated so far, empty until the first consolidation.
    summary: String,
    /// Entries as they were written, oldest first.
    entries: Vec<String>,
}

impl Scope {
    /// Read *path*; an absent file loads as an empty scope.
    fn load(path: &Path) -> Result<Scope> {
        if !path.exists() {
            return Ok(Scope::default());
        }
        let text = fs::read_to_string(path)
            .map_err(|err| Error::Io(format!("{}: {err}", path.display())))?;
        let payload: Value = serde_json::from_str(&text)
            .map_err(|err| Error::Value(format!("{}: {err}", path.display())))?;
        Ok(Scope {
            summary: payload["summary"].as_str().unwrap_or_default().to_string(),
            entries: payload["entries"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Write the whole scope to *path*, creating the root if it is not there.
    fn save(&self, path: &Path) -> Result<()> {
        let payload = json!({"summary": self.summary, "entries": self.entries});
        write_creating_parent(path, &payload.to_string())
    }

    /// The scope as a prompt should quote it: the consolidation, then the
    /// entries that have not been folded into it yet.
    fn render(&self) -> String {
        let mut blocks = Vec::with_capacity(self.entries.len() + 1);
        if !self.summary.is_empty() {
            blocks.push(format!("{CONSOLIDATED_PREFIX}\n{}", self.summary));
        }
        blocks.extend(self.entries.iter().cloned());
        blocks.join("\n\n")
    }
}

impl SummarizingMemory {
    /// Keep every scope under *root*, consolidating through *model* on
    /// *provider*.
    ///
    /// The provider is required rather than optional. A store built without one
    /// would keep every entry forever, which is what [`FileMemory`] already
    /// does and not what this store is chosen for, and it would do it silently.
    pub fn new(
        root: impl Into<PathBuf>,
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
    ) -> Self {
        SummarizingMemory {
            root: root.into(),
            provider,
            model: model.into(),
            keep: DEFAULT_KEEP_ENTRIES,
            scopes: Mutex::new(HashMap::new()),
        }
    }

    /// Keep *entries* verbatim rather than [`DEFAULT_KEEP_ENTRIES`].
    ///
    /// The one figure the store cannot pick for a caller: how much recent
    /// detail a harness needs word for word depends on what its agents write,
    /// and a session of long notes and one of one-line facts want different
    /// answers.
    pub fn with_keep(mut self, entries: usize) -> Self {
        self.keep = entries;
        self
    }

    /// The file *scope* is kept in, under the store's root. See
    /// [`scope_file`], which is where the scope stops being a path.
    fn file(&self, scope: &str) -> PathBuf {
        scope_file(&self.root, scope, "json")
    }

    /// Run *act* against *scope*, loading it if this is its first use.
    fn with<T>(&self, scope: &str, act: impl FnOnce(&mut Scope) -> Result<T>) -> Result<T> {
        let mut scopes = self.scopes.lock().unwrap_or_else(|err| err.into_inner());
        if !scopes.contains_key(scope) {
            scopes.insert(scope.to_string(), Scope::load(&self.file(scope))?);
        }
        act(scopes.get_mut(scope).expect("inserted above"))
    }

    /// Ask the model to fold *overflow* into *summary*, or `None` to leave the
    /// scope as it is.
    ///
    /// A provider failure is `None` rather than an error, on
    /// [`crate::compaction`]'s reasoning inverted: there, a failed summary
    /// means keeping turns that would have been dropped; here it means keeping
    /// notes that would have been rewritten. Both preserve what was actually
    /// written, and losing a run's notes to a network error is the worse of the
    /// two outcomes by a distance.
    fn consolidate(&self, summary: &str, overflow: &[String]) -> Result<Option<String>> {
        let mut blocks = Vec::with_capacity(overflow.len() + 1);
        if !summary.is_empty() {
            blocks.push(summary.to_string());
        }
        blocks.extend(overflow.iter().cloned());
        let messages = [
            json!({"role": "system", "content": CONSOLIDATE_PROMPT}),
            json!({"role": "user", "content": blocks.join("\n\n")}),
        ];
        match self.provider.chat_with_retries(
            &self.model,
            &messages,
            "memory consolidation",
            None,
            ReasoningEffort::default(),
        ) {
            Ok(response) if response.content.trim().is_empty() => Ok(None),
            Ok(response) => Ok(Some(response.content.trim().to_string())),
            Err(error) if error.is_provider() => {
                logging::warning(&format!(
                    "Memory consolidation failed; keeping the notes as written: {error}"
                ));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

impl MemoryStore for SummarizingMemory {
    fn read(&self, scope: &str) -> Result<String> {
        self.with(scope, |held| Ok(held.render()))
    }

    fn append(&self, scope: &str, note: &str) -> Result<()> {
        let entry = note.trim();
        if entry.is_empty() {
            return Ok(());
        }
        let path = self.file(scope);
        self.with(scope, |held| {
            held.entries.push(entry.to_string());
            held.save(&path)
        })
    }

    fn open(&self, scope: &str) -> Result<()> {
        self.with(scope, |_| Ok(()))
    }

    fn age(&self, scope: &str) -> Option<u64> {
        days_since_write(&self.file(scope))
    }

    fn path(&self, scope: &str) -> Option<PathBuf> {
        Some(self.file(scope))
    }

    /// Fold every overflowing scope's oldest entries into its summary.
    ///
    /// The overflowing scopes are collected and the lock dropped before the
    /// first provider call, because a provider written by the caller may reach
    /// back into this store — reading its own memory to decide what to say is
    /// exactly what a store like this invites — and holding the lock across the
    /// call would make that a deadlock rather than a re-entry.
    fn close(&self) -> Result<()> {
        let overflowing: Vec<(String, String, Vec<String>)> = {
            let scopes = self.scopes.lock().unwrap_or_else(|err| err.into_inner());
            scopes
                .iter()
                .filter(|(_, held)| held.entries.len() > self.keep)
                .map(|(name, held)| {
                    let cut = held.entries.len() - self.keep;
                    (
                        name.clone(),
                        held.summary.clone(),
                        held.entries[..cut].to_vec(),
                    )
                })
                .collect()
        };

        for (scope, summary, overflow) in overflowing {
            let Some(consolidated) = self.consolidate(&summary, &overflow)? else {
                continue;
            };
            let path = self.file(&scope);
            self.with(&scope, |held| {
                held.summary = consolidated;
                // By count rather than by content: the entries handed to the
                // summarizer were the oldest, and nothing removes an entry.
                held.entries.drain(..overflow.len().min(held.entries.len()));
                held.save(&path)
            })?;
        }
        Ok(())
    }
}

/// The default character ceiling on one scope of a [`CuratedMemory`].
///
/// Roughly 550 tokens: enough for the eight to fifteen standing facts a run
/// actually needs, and small enough that carrying it in every prompt of every
/// turn is not the largest thing the session pays for.
pub const DEFAULT_MEMORY_BUDGET: usize = 2_200;

/// The line that separates one entry from the next, on disk and in the prompt.
///
/// A delimiter rather than the blank line [`Memory::append_entry`] uses,
/// because entries here are addressed individually: a note that contains a
/// blank line of its own would otherwise become two entries, and the second
/// would be revisable separately from the first. `§` is not Markdown, so it
/// cannot be confused for structure inside a note.
pub const ENTRY_SEPARATOR: &str = "§";

/// A store with a character ceiling, curated by the agents that write to it.
///
/// One text file per scope under *root*, holding entries separated by
/// [`ENTRY_SEPARATOR`]. What makes it different from the other two bundled
/// stores is what happens when a scope fills: nothing is dropped and nothing is
/// rewritten behind the agents' backs. An [`append`](MemoryStore::append) that
/// would cross the ceiling fails, and the error says what is stored and what to
/// do about it, so the agent merges two entries or removes one through
/// [`revise`](MemoryStore::revise) and writes its note again inside the same
/// turn.
///
/// That is the trade against [`SummarizingMemory`], which bounds a scope by
/// spending a provider call at the end of the run. Here the bound costs no call
/// at all and the model doing the curating is the one that wrote the notes and
/// knows which of them still matter — but it costs the agent a refusal it has
/// to handle, and an agent that ignores the refusal keeps the memory it has
/// rather than making room.
///
/// Two smaller rules follow from entries being addressable. A note already
/// stored word for word is accepted and not stored twice, because a model
/// re-recording a fact it already recorded has not failed at anything. And
/// [`read`](MemoryStore::read) leads with how full the scope is, so the agent
/// can see the ceiling coming rather than only meet it.
pub struct CuratedMemory {
    root: PathBuf,
    budget: usize,
    /// Entries per scope, oldest first, loaded on demand. `Mutex` rather than
    /// `RwLock` for the reason the other two stores use one: a read of an
    /// unloaded scope loads it.
    scopes: Mutex<HashMap<String, Vec<String>>>,
}

impl CuratedMemory {
    /// Keep every scope under *root*, each bounded by
    /// [`DEFAULT_MEMORY_BUDGET`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        CuratedMemory {
            root: root.into(),
            budget: DEFAULT_MEMORY_BUDGET,
            scopes: Mutex::new(HashMap::new()),
        }
    }

    /// Bound each scope at *characters* rather than at the default.
    ///
    /// The figure is a share of the context window, so what it should be
    /// depends on the model and on how much of the prompt the harness has
    /// already spent on roles, skills, and context.
    pub fn with_budget(mut self, characters: usize) -> Self {
        self.budget = characters;
        self
    }

    /// The file *scope* is kept in, under the store's root.
    fn file(&self, scope: &str) -> PathBuf {
        scope_file(&self.root, scope, "md")
    }

    /// Run *act* against *scope*, loading it if this is its first use.
    fn with<T>(&self, scope: &str, act: impl FnOnce(&mut Vec<String>) -> Result<T>) -> Result<T> {
        let mut scopes = self.scopes.lock().unwrap_or_else(|err| err.into_inner());
        if !scopes.contains_key(scope) {
            scopes.insert(scope.to_string(), self.load(scope)?);
        }
        act(scopes.get_mut(scope).expect("inserted above"))
    }

    /// Read *scope*'s file into entries; an absent file loads as none.
    ///
    /// Blank entries are dropped rather than kept, so a file a person edited by
    /// hand — leaving a trailing separator, or two in a row — loads as the
    /// entries it visibly holds.
    fn load(&self, scope: &str) -> Result<Vec<String>> {
        let path = self.file(scope);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path)
            .map_err(|err| Error::Io(format!("{}: {err}", path.display())))?;
        Ok(text
            .split(&format!("\n{ENTRY_SEPARATOR}\n"))
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The entries as they are written to disk and quoted into a prompt.
    fn render(entries: &[String]) -> String {
        entries.join(&format!("\n{ENTRY_SEPARATOR}\n"))
    }

    /// How many characters *entries* occupy, separators included.
    ///
    /// The rendering rather than the notes alone, because the rendering is what
    /// the prompt actually carries and the ceiling is there to bound the prompt.
    fn used(entries: &[String]) -> usize {
        CuratedMemory::render(entries).chars().count()
    }

    /// Store *entries* as *scope*'s whole file.
    fn save(&self, scope: &str, entries: &[String]) -> Result<()> {
        write_creating_parent(&self.file(scope), &CuratedMemory::render(entries))
    }

    /// A refusal that carries what is stored, so the agent can act on it
    /// without a read first.
    ///
    /// The whole point of failing rather than trimming is that the agent
    /// decides what goes; an error that does not say what is there would be
    /// telling it to decide blind.
    fn full(&self, would_be: usize, entries: &[String]) -> Error {
        Error::Value(format!(
            "Memory is full: that would take this scope to {would_be} of its {} \
             characters, so nothing was stored. Merge two entries into a \
             shorter one, or remove an entry that no longer matters, then write \
             it again. Stored now:\n\n{}",
            self.budget,
            CuratedMemory::render(entries)
        ))
    }

    /// The index of the one entry containing *fragment*, or an error naming
    /// why there is not exactly one.
    ///
    /// Matching several is refused rather than resolved by taking the first:
    /// the agent gave a fragment it believed was unique, and rewriting the
    /// wrong entry silently is the one outcome it cannot detect.
    fn locate(&self, entries: &[String], fragment: &str) -> Result<usize> {
        let matched: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.contains(fragment))
            .map(|(index, _)| index)
            .collect();
        match matched.as_slice() {
            [only] => Ok(*only),
            [] => Err(Error::Value(format!(
                "No entry in memory contains that text, so nothing was changed. \
                 Stored now:\n\n{}",
                CuratedMemory::render(entries)
            ))),
            several => Err(Error::Value(format!(
                "{} entries contain that text, so nothing was changed. Give a \
                 longer fragment that appears in only one. Stored now:\n\n{}",
                several.len(),
                CuratedMemory::render(entries)
            ))),
        }
    }
}

impl MemoryStore for CuratedMemory {
    /// The entries, led by how full the scope is; empty while it holds nothing.
    ///
    /// Empty and not a bare usage line, because
    /// [`memory_block`](crate::prompting::memory_block) renders nothing for
    /// empty content, and a session whose agents have written nothing yet
    /// should carry no memory section rather than one saying so.
    fn read(&self, scope: &str) -> Result<String> {
        self.with(scope, |entries| {
            if entries.is_empty() {
                return Ok(String::new());
            }
            Ok(format!(
                "({} of {} characters used, {} entries)\n\n{}",
                CuratedMemory::used(entries),
                self.budget,
                entries.len(),
                CuratedMemory::render(entries)
            ))
        })
    }

    fn append(&self, scope: &str, note: &str) -> Result<()> {
        let entry = note.trim();
        if entry.is_empty() {
            return Ok(());
        }
        self.with(scope, |entries| {
            if entries.iter().any(|stored| stored == entry) {
                return Ok(());
            }
            let mut proposed = entries.clone();
            proposed.push(entry.to_string());
            let used = CuratedMemory::used(&proposed);
            if used > self.budget {
                return Err(self.full(used, entries));
            }
            self.save(scope, &proposed)?;
            *entries = proposed;
            Ok(())
        })
    }

    fn revise(&self, scope: &str, old: &str, new: &str) -> Result<()> {
        let fragment = old.trim();
        if fragment.is_empty() {
            return Err(Error::Value(
                "An entry is addressed by a fragment of its own text, and none \
                 was given, so nothing was changed."
                    .to_string(),
            ));
        }
        let replacement = new.trim();
        self.with(scope, |entries| {
            let found = self.locate(entries, fragment)?;
            let mut proposed = entries.clone();
            if replacement.is_empty() {
                proposed.remove(found);
            } else {
                proposed[found] = replacement.to_string();
                let used = CuratedMemory::used(&proposed);
                if used > self.budget {
                    return Err(self.full(used, entries));
                }
            }
            self.save(scope, &proposed)?;
            *entries = proposed;
            Ok(())
        })
    }

    fn open(&self, scope: &str) -> Result<()> {
        self.with(scope, |_| Ok(()))
    }

    fn age(&self, scope: &str) -> Option<u64> {
        days_since_write(&self.file(scope))
    }

    fn path(&self, scope: &str) -> Option<PathBuf> {
        Some(self.file(scope))
    }

    fn budget(&self) -> Option<usize> {
        Some(self.budget)
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

    // ---- SummarizingMemory ----------------------------------------------

    use crate::provider::{ProviderBase, ProviderResponse};
    use crate::tooling::ToolSpec;

    /// Answers every call with one fixed reply, recording what it was asked.
    struct StubProvider {
        base: ProviderBase,
        reply: Result<String>,
        prompts: Mutex<Vec<String>>,
    }

    impl StubProvider {
        fn saying(reply: &str) -> Self {
            StubProvider {
                base: ProviderBase::new(0, 0.0, None),
                reply: Ok(reply.to_string()),
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            StubProvider {
                base: ProviderBase::new(0, 0.0, None),
                reply: Err(Error::Provider("no route to host".to_string())),
                prompts: Mutex::new(Vec::new()),
            }
        }

        /// The user message of every request, in order.
        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().expect("lock").clone()
        }
    }

    impl Provider for StubProvider {
        fn name(&self) -> &str {
            "StubProvider"
        }

        fn base(&self) -> &ProviderBase {
            &self.base
        }

        fn chat(
            &self,
            _model: &str,
            messages: &[Value],
            _tools: Option<&[ToolSpec]>,
            _effort: ReasoningEffort,
        ) -> Result<ProviderResponse> {
            self.prompts.lock().expect("lock").push(
                messages
                    .last()
                    .and_then(|message| message["content"].as_str())
                    .unwrap_or_default()
                    .to_string(),
            );
            match &self.reply {
                Ok(text) => Ok(ProviderResponse::text(text.clone())),
                Err(error) => Err(Error::Provider(error.to_string())),
            }
        }
    }

    fn summarizing(dir: &TempDir, provider: Arc<StubProvider>) -> SummarizingMemory {
        SummarizingMemory::new(dir.join("memory"), provider, "test-model")
    }

    #[test]
    fn entries_read_back_verbatim_until_something_consolidates_them() {
        let dir = TempDir::new("summarizing-verbatim");
        let provider = Arc::new(StubProvider::saying("unused"));
        let store = summarizing(&dir, Arc::clone(&provider)).with_keep(2);

        store.append("shared", "first").expect("append");
        store.append("shared", "  second  ").expect("append");
        store.append("shared", "   ").expect("blank");

        assert_eq!(store.read("shared").expect("read"), "first\n\nsecond");
        assert!(
            provider.prompts().is_empty(),
            "nothing has overflowed, so nothing was summarized"
        );
    }

    #[test]
    fn closing_folds_everything_past_the_kept_entries_into_one_summary() {
        let dir = TempDir::new("summarizing-close");
        let provider = Arc::new(StubProvider::saying("Alice chose the blue one."));
        let store = summarizing(&dir, Arc::clone(&provider)).with_keep(2);

        for note in ["oldest", "older", "recent", "newest"] {
            store.append("shared", note).expect("append");
        }
        store.close().expect("close");

        assert_eq!(
            store.read("shared").expect("read"),
            format!("{CONSOLIDATED_PREFIX}\nAlice chose the blue one.\n\nrecent\n\nnewest")
        );
        assert_eq!(
            provider.prompts(),
            vec!["oldest\n\nolder".to_string()],
            "one call, carrying only what overflowed"
        );
    }

    #[test]
    fn a_second_consolidation_is_given_the_first_one_to_build_on() {
        let dir = TempDir::new("summarizing-again");
        let provider = Arc::new(StubProvider::saying("everything so far"));
        let store = summarizing(&dir, Arc::clone(&provider)).with_keep(1);

        store.append("shared", "one").expect("append");
        store.append("shared", "two").expect("append");
        store.close().expect("close");
        store.append("shared", "three").expect("append");
        store.close().expect("close");

        assert_eq!(
            provider.prompts(),
            vec!["one".to_string(), "everything so far\n\ntwo".to_string()],
            "the running summary leads the second request, so what it already \
             covers is not consolidated from scratch"
        );
    }

    #[test]
    fn a_failed_consolidation_keeps_the_notes_as_they_were_written() {
        let dir = TempDir::new("summarizing-failure");
        let provider = Arc::new(StubProvider::failing());
        let store = summarizing(&dir, Arc::clone(&provider)).with_keep(1);

        store.append("shared", "one").expect("append");
        store.append("shared", "two").expect("append");
        store
            .close()
            .expect("a provider failure is not a session failure");

        assert_eq!(store.read("shared").expect("read"), "one\n\ntwo");
    }

    #[test]
    fn a_scope_survives_the_store_that_wrote_it() {
        let dir = TempDir::new("summarizing-reopen");
        let provider = Arc::new(StubProvider::saying("what went before"));
        let store = summarizing(&dir, Arc::clone(&provider)).with_keep(1);

        store.append("shared", "one").expect("append");
        store.append("shared", "two").expect("append");
        store.close().expect("close");

        let reopened = summarizing(&dir, Arc::clone(&provider)).with_keep(1);
        assert_eq!(
            reopened.read("shared").expect("read"),
            format!("{CONSOLIDATED_PREFIX}\nwhat went before\n\ntwo")
        );
    }

    #[test]
    fn a_scope_is_a_key_and_never_a_path_out_of_the_root() {
        let dir = TempDir::new("summarizing-scope");
        let provider = Arc::new(StubProvider::saying("unused"));
        let store = summarizing(&dir, provider);
        let root = dir.join("memory");

        let escape = store.path("../../elsewhere").expect("a file per scope");
        assert_eq!(escape.parent(), Some(root.as_path()));
        assert_eq!(
            escape.file_name().and_then(|name| name.to_str()),
            Some("%2E%2E%2F%2E%2E%2Felsewhere.json")
        );
        assert_ne!(
            store.path("a/b"),
            store.path("a_b"),
            "encoding is reversible, so two scopes never share one file"
        );
    }

    #[test]
    fn a_scope_is_dated_by_its_file_and_undated_before_there_is_one() {
        let dir = TempDir::new("summarizing-age");
        let provider = Arc::new(StubProvider::saying("unused"));
        let store = summarizing(&dir, provider);

        assert_eq!(store.age("shared"), None, "there is no file to date yet");
        store.open("shared").expect("nothing on disk to open");
        assert_eq!(store.age("shared"), None, "opening writes nothing");
        store.append("shared", "now").expect("append");
        assert_eq!(store.age("shared"), Some(0));
    }

    // ---- CuratedMemory ---------------------------------------------------

    #[test]
    fn a_store_with_no_ceiling_refuses_to_revise_and_says_so() {
        let store = Ephemeral::default();
        store.append("anything", "kept").expect("append");

        assert_eq!(store.budget(), None, "nothing is being curated towards");
        let refusal = store
            .revise("anything", "kept", "kept, revised")
            .expect_err("an append-only store cannot address an entry");
        assert!(
            refusal.to_string().contains("cannot be revised"),
            "the agent is told why, not merely that it failed: {refusal}"
        );
    }

    #[test]
    fn entries_read_back_behind_a_line_saying_how_full_the_scope_is() {
        let dir = TempDir::new("curated-usage");
        let store = CuratedMemory::new(dir.join("memory"));

        assert_eq!(
            store.read("shared").expect("read"),
            "",
            "an empty scope renders no block at all, not a block saying it is empty"
        );

        store.append("shared", "first").expect("append");
        store.append("shared", "  second  ").expect("append");
        store.append("shared", "   ").expect("blank");

        assert_eq!(
            store.read("shared").expect("read"),
            format!(
                "(14 of {DEFAULT_MEMORY_BUDGET} characters used, 2 entries)\n\n\
                 first\n{ENTRY_SEPARATOR}\nsecond"
            ),
            "the separators count, because they are in the prompt too"
        );
    }

    #[test]
    fn a_note_already_stored_word_for_word_is_accepted_and_not_stored_twice() {
        let dir = TempDir::new("curated-duplicate");
        let store = CuratedMemory::new(dir.join("memory"));

        store.append("shared", "Alice chose blue").expect("append");
        store
            .append("shared", "  Alice chose blue  ")
            .expect("re-recording a known fact is not a failure");

        assert!(store.read("shared").expect("read").contains("1 entries"));
    }

    #[test]
    fn an_append_past_the_ceiling_is_refused_and_says_what_is_stored() {
        let dir = TempDir::new("curated-full");
        let store = CuratedMemory::new(dir.join("memory")).with_budget(20);

        store.append("shared", "0123456789").expect("append");
        let refusal = store
            .append("shared", "abcdefghij")
            .expect_err("21 characters against a ceiling of 20");

        let said = refusal.to_string();
        assert!(said.contains("23 of its 20 characters"), "{said}");
        assert!(
            said.contains("0123456789"),
            "the agent is told what to merge or remove: {said}"
        );
        assert_eq!(
            store.read("shared").expect("read"),
            "(10 of 20 characters used, 1 entries)\n\n0123456789",
            "the refused note was not stored, and nothing else moved"
        );
    }

    #[test]
    fn revising_replaces_the_whole_entry_a_fragment_addresses() {
        let dir = TempDir::new("curated-revise");
        let store = CuratedMemory::new(dir.join("memory"));

        store.append("shared", "Alice chose blue").expect("append");
        store.append("shared", "Bob chose green").expect("append");
        store
            .revise("shared", "Alice", "Alice and Bob both chose blue")
            .expect("revise");

        assert_eq!(
            store.read("shared").expect("read"),
            format!(
                "(47 of {DEFAULT_MEMORY_BUDGET} characters used, 2 entries)\n\n\
                 Alice and Bob both chose blue\n{ENTRY_SEPARATOR}\nBob chose green"
            ),
            "the matched entry is replaced entirely, not the matched fragment"
        );
    }

    #[test]
    fn revising_to_nothing_removes_the_entry() {
        let dir = TempDir::new("curated-remove");
        let store = CuratedMemory::new(dir.join("memory"));

        store.append("shared", "still true").expect("append");
        store.append("shared", "no longer true").expect("append");
        store.revise("shared", "no longer", "  ").expect("remove");

        assert_eq!(
            store.read("shared").expect("read"),
            format!("(10 of {DEFAULT_MEMORY_BUDGET} characters used, 1 entries)\n\nstill true")
        );
    }

    #[test]
    fn a_fragment_matching_none_or_several_changes_nothing_and_names_which() {
        let dir = TempDir::new("curated-ambiguous");
        let store = CuratedMemory::new(dir.join("memory"));

        store.append("shared", "Alice chose blue").expect("append");
        store.append("shared", "Alice chose again").expect("append");

        let several = store
            .revise("shared", "Alice", "one entry now")
            .expect_err("two entries contain it");
        assert!(
            several.to_string().contains("2 entries contain"),
            "{several}"
        );

        let none = store
            .revise("shared", "Carol", "who?")
            .expect_err("no entry contains it");
        assert!(none.to_string().contains("No entry"), "{none}");

        let unaddressed = store
            .revise("shared", "  ", "anything")
            .expect_err("nothing was given to match on");
        assert!(
            unaddressed.to_string().contains("fragment"),
            "{unaddressed}"
        );

        assert!(
            store.read("shared").expect("read").contains("2 entries"),
            "three refusals, and the scope is as it was"
        );
    }

    #[test]
    fn a_revision_past_the_ceiling_is_refused_and_the_entry_survives() {
        let dir = TempDir::new("curated-revise-full");
        let store = CuratedMemory::new(dir.join("memory")).with_budget(20);

        store.append("shared", "short").expect("append");
        let refusal = store
            .revise("shared", "short", "far too long to fit in twenty")
            .expect_err("the replacement overflows");

        assert!(refusal.to_string().contains("of its 20 characters"));
        assert_eq!(
            store.read("shared").expect("read"),
            "(5 of 20 characters used, 1 entries)\n\nshort"
        );
    }

    #[test]
    fn a_curated_scope_survives_the_store_that_wrote_it() {
        let dir = TempDir::new("curated-reopen");
        let root = dir.join("memory");
        let store = CuratedMemory::new(&root);

        store.append("shared", "first").expect("append");
        store.append("shared", "second").expect("append");
        store.revise("shared", "first", "").expect("remove");

        let path = store.path("shared").expect("a file per scope");
        assert_eq!(fs::read_to_string(&path).expect("read back"), "second");
        assert_eq!(
            CuratedMemory::new(&root).read("shared").expect("read"),
            format!("(6 of {DEFAULT_MEMORY_BUDGET} characters used, 1 entries)\n\nsecond")
        );
    }

    #[test]
    fn a_hand_edited_file_loads_as_the_entries_it_visibly_holds() {
        let dir = TempDir::new("curated-handwritten");
        let root = dir.join("memory");
        let store = CuratedMemory::new(&root);
        let path = store.path("shared").expect("a file per scope");

        write_creating_parent(
            &path,
            &format!("first\n{ENTRY_SEPARATOR}\n\n{ENTRY_SEPARATOR}\n  second  \n"),
        )
        .expect("write by hand");

        assert!(
            store
                .read("shared")
                .expect("read")
                .ends_with(&format!("first\n{ENTRY_SEPARATOR}\nsecond")),
            "the empty entry between the two separators is not one"
        );
    }

    #[test]
    fn a_curated_scope_is_a_key_and_never_a_path_out_of_the_root() {
        let dir = TempDir::new("curated-scope");
        let root = dir.join("memory");
        let store = CuratedMemory::new(&root);

        let escape = store.path("../../elsewhere").expect("a file per scope");
        assert_eq!(escape.parent(), Some(root.as_path()));
        assert_eq!(
            escape.file_name().and_then(|name| name.to_str()),
            Some("%2E%2E%2F%2E%2E%2Felsewhere.md")
        );
    }

    #[test]
    fn a_curated_scope_is_dated_by_its_file_and_undated_before_there_is_one() {
        let dir = TempDir::new("curated-age");
        let store = CuratedMemory::new(dir.join("memory"));

        assert_eq!(store.age("shared"), None, "there is no file to date yet");
        store.open("shared").expect("nothing on disk to open");
        assert_eq!(store.age("shared"), None, "opening writes nothing");
        store.append("shared", "now").expect("append");
        assert_eq!(store.age("shared"), Some(0));
    }
}
