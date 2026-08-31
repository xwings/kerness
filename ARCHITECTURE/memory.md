# Memory

## Goal

What the agents in a session remember, and where it is kept. It is the only
state that outlives a turn without being in the conversation, and what an agent
writes there goes into the next agent's prompt verbatim.

Memory is a slot. A session holds one `MemoryStore` and addresses it by *scope*
— a name whose meaning is the store's to decide. The default store, `FileMemory`,
reads a scope as a path and keeps free-form Markdown prose there rather than a
key-value structure. A caller who wants a database, an embedding index, or a
summarising store installs one and the session is unchanged.

Three consequences follow from *verbatim*, and all three are the module's
business rather than its callers':

- Memory is a channel between agents. Text arriving from a model is filtered on
  the way in and quoted on the way out.
- Notes outlive the run that wrote them. A session resumed a week later reads
  week-old notes, so the scope's age travels with its content into the prompt.
- A store is not trusted to be a file. `path()` is how one that *is* opts into
  the workspace confinement the session file goes through.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the store trait, the default store, the file primitive, and the filter trait |
| `crates/kerness/src/session.rs:226` | `Memories` — the store plus the scope each agent addresses it by |
| `crates/kerness/src/session.rs:267` | `remember`, the one path model output takes into memory |
| `bindings/python/src/memory.rs` | the boundary: a Python store seen as one, and the bundled store seen from Python |
| `bindings/python/src/session.rs:176` | `PyFilter`, a Python callable behind the filter trait |
| `bindings/python/src/types.rs:972` | `PyMemory`, the file primitive on its own |
| `bindings/python/kerness/memory.py` | the `MemoryStore` ABC and the re-exports |

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:79` — `MemoryStore` — `read(scope)` and
  `append(scope, note)` are required; `open`, `age`, `path`, and `close` are
  defaulted, because they are the four a store can honestly have no answer for.
- `crates/kerness/src/memory.rs:99` — `open(scope)` — called once per scope at
  the top of `run()`. The point is *when*: a store that cannot reach what it is
  backed by says so before the first provider call rather than mid-turn.
- `crates/kerness/src/memory.rs:133` — `close()` — called once at the end of
  `run()`, after the session result. The only moment at which the whole run is
  known and nothing further will be appended, so it is where a store that
  consolidates does it.
- `crates/kerness/src/memory.rs:144` — `FileMemory` — the default: one `Memory`
  per scope behind a `Mutex`, opened on demand. `Mutex` and not `RwLock` because
  reading an unopened scope loads it, which is a write to the map.
- `crates/kerness/src/memory.rs:204` — `Memory` — a path and the loaded content,
  the file primitive `FileMemory` is built out of and usable on its own.
  `load()` at `:251` treats a missing file as empty content rather than an error;
  `append_entry(text)` at `:278` adds the entry separator and is what every write
  in a session goes through; `age()` at `:237` is whole days since the file was
  last written.
- `crates/kerness/src/memory.rs:54` — `MemoryFilter` — one method,
  `filter(note, actor)`, returning the text to store or `None` to drop it.
- `crates/kerness/src/session.rs:234` — `scope_for(agent)` — the agent's own
  scope when it declared one, the session's otherwise. This is what makes memory
  shared by default and private only on request.
- `bindings/python/src/memory.rs:109` — `bind_memory_store(object)` — a Python
  store seen as a `MemoryStore`, with an exact-type shortcut past `FileMemory`.
- `bindings/python/src/memory.rs:173` — `PySessionMemory` — what
  `session.memory` returns: the live store at the session scope, not a snapshot.

### Scope, and why it is a string

`SessionConfig::memory` and `Agent::memory` are strings the session never
parses. `FileMemory` reads one as a path; another store reads it as a key, a
collection name, or a namespace. Nothing above `MemoryStore` assumes memory is a
file, which is what lets a store be replaced without the session, the prompt
assembler, or the two memory tools changing.

The session takes the store and the scope together, under one lock, and then
calls the store with the lock released — `store_for` (`session.rs:298`). A store
written in Python can run arbitrary code, including code that re-enters the
session; holding the session's lock across that call is how it would deadlock.

### Age, read from the filesystem

`FileMemory::age` (`memory.rs:193`) reads the mtime rather than parsing the
content, because the module imposes no format on the content and a timestamp
parsed out of prose would be one. A clock that has gone backwards since the
write reads as `0` rather than as an error: staleness is advisory, and no caveat
is the better answer when the age is not credible.

`None` and `Some(0)` are distinct and both reach the prompt.
[prompting.md](prompting.md)'s `memory_block` takes the age and renders a caveat
past `MEMORY_STALE_AFTER_DAYS`; a scope with no file yet holds only notes written
this run, which are as fresh as the run, so it carries none. A store with no
notion of a write time takes the trait's default and is correct.

### The trust boundary

What an agent writes lands inside another agent's *system prompt*, which is the
position a session's own instructions occupy. Two separate mechanisms answer
that, at the two ends:

- On the way in, `MemoryFilter` (`memory.rs:54`). It is applied in `remember`
  (`session.rs:267`), a free function both write paths call — the `write_memory`
  tool (`session.rs:2035`) and the `@MEMORY:` marker pass (`session.rs:1522`) —
  so a caller who installs a filter cannot have it cover one and miss the other.
  A dropped note is reported to the writer as *not saved*, without saying which
  rule refused it: a specific rejection teaches a model how to word the next
  attempt.
- On the way out, `MEMORY_CAVEAT` and the `MEMORY_BEGIN`/`MEMORY_END` fence in
  [prompting.md](prompting.md). The block says plainly that it is recorded
  material, not instruction.

The filter runs *before* the store sees a note, and the store is reached only
through `remember`. That ordering is the whole reason a third-party store is
safe to install: it cannot be a route around a filter the caller put in.

The framework ships no filter implementation. What counts as a secret, and what
a session is willing to persist, are the caller's to define; a redactor guessing
at it here would be wrong in both directions and wrong silently.

Two things are deliberately outside the boundary. A caller writing through
`Memory` or `session.memory` directly is not filtered, because the caller is not
the untrusted party — and neither is the session's own closing
`## Session Result` block (`session.rs:1016`), which the framework composes
rather than a model. From Python, a filter that raises drops the note and logs a
warning (`bindings/python/src/session.rs:192`): the trait has no error path, and
the safe reading of a filter that could not decide is that the note stays out —
but silence would make that indistinguishable from a deliberate `None`.

### Failing early, and failing soft

The two directions are deliberate and different.

`open()` is fallible and runs before the first turn, so an unreachable store
costs nothing (`session.rs:976`). `append()` and `close()` are fallible and
propagate, because a note that was not stored is a result the caller must see.

`memory_text` (`session.rs:385`) is the exception: it logs and yields an empty
block rather than failing the run. `PromptAssembler` takes an infallible
`Fn(&Agent) -> String`, and a read that failed *this late* would discard provider
calls already paid for — every scope was opened successfully before the first
turn, so a failure here arose mid-run.

The two infallible trait methods take the same shape at the binding. `PyStore`'s
`optional` (`bindings/python/src/memory.rs:40`) logs and answers `None` when a
Python `age` or `path` raises: the trait returns `Option`, not `Result`, and the
honest reading of a store that cannot name its file is a store that names none —
which is what a store keeping nothing on disk answers anyway.

### The Python surface

`MemoryStore` is an ABC in `bindings/python/kerness/memory.py:19` for the reason
`Channel` is one ([bindings.md](bindings.md)): it is what callers subclass, and
an extension cannot declare an abstract base class. `FileMemory` is registered
against it at `:78` rather than inheriting from it, so `isinstance` holds without
an extension type subclassing a Python ABC.

`session.memory` returns `PySessionMemory`, not a `Memory`. The distinction is
observable — `session.memory.read()` after `run()` has to show what the run
wrote, which a snapshot taken at construction would not — and it is honest about
the store: `path` and `age` are `None` when the store keeps no file, and there is
no `write()`, because replacing everything is not something the trait offers.

`PromptAssembler`'s `memory_for` callback is live for the same reason: it is
called per turn, so two agents pointed at one scope both see each other's writes,
and a scope written mid-run stops being stale without the session rebuilding
anything.

## Interactions

- Rendered into a system prompt by [prompting.md](prompting.md)'s `memory_block`,
  which also owns the caveat and the staleness line.
- Held per session by [session.md](session.md)'s `Memories` (`session.rs:226`),
  which also holds the configured `memory_filter`.
- Confined by [access.md](access.md): whatever `path(scope)` names is checked
  against the workspace at `Session::new` (`session.rs:565`) and, for a per-agent
  scope, at the top of `run()` (`session.rs:958`).
- Read and written through the `read_memory` and `write_memory` tools registered
  by [toolkit.md](toolkit.md).
- Memory markers in a reply are extracted by [utils.md](utils.md)'s
  `parse_memory_markers`.
- Counted as prompt overhead by [compaction.md](compaction.md): memory is part of
  the system message, so it narrows what the conversation may use.

## How to Test

```sh
cargo test -p kerness --lib memory                                  # pass = 0 failed
cargo test -p kerness --lib session                                 # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_memory.py -q  # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q # pass = 0 failed
```

- `crates/kerness/src/memory.rs:370` —
  `a_file_written_now_is_zero_days_old_and_an_absent_one_has_no_age` — the two
  cases the prompt distinguishes.
- `crates/kerness/src/memory.rs:394` — `the_default_store_keeps_one_file_per_scope`
  and `:413` `a_scope_never_opened_still_reads_what_is_on_disk`, which is the
  lazy load in `FileMemory::with` proving a caller need not call `open`.
- `crates/kerness/src/memory.rs:454` —
  `a_store_writing_no_file_answers_the_defaults_and_leaves_no_trace` — the
  smallest possible store, and the four defaults answering for it.
- `crates/kerness/src/session.rs:3120` —
  `an_installed_store_is_opened_read_written_and_closed` — the slot end to end,
  including that every `open` precedes every `read` and that a per-agent scope
  routes to itself.
- `crates/kerness/src/session.rs:3187` —
  `the_filter_runs_before_an_installed_store_sees_a_note` — the ordering the
  trust boundary rests on: two calls to the tool, one append.
- `crates/kerness/src/session.rs:3252` —
  `a_store_that_names_no_file_is_checked_against_no_workspace` — the default's
  path refused, a store answering `None` left alone. `:3281`
  `a_store_that_cannot_open_stops_the_run_before_the_first_turn` is the other
  half: no provider was called.
- `crates/kerness/src/session.rs:3016` —
  `a_filter_rewrites_or_drops_what_an_agent_writes` — the filter at the layer
  that applies it, not through a whole session.
- `bindings/python/tests/test_memory.py:13` — `test_load_reads_what_is_there_and_creates_what_is_not`;
  `:49` `test_an_entry_is_stored_verbatim_one_blank_line_apart`; `:69`
  `test_nothing_reaches_disk_until_there_is_something_to_write`; and `:83`
  `test_age_is_none_without_a_file_and_whole_days_once_there_is_one`, which
  backdates an mtime rather than waiting a day, and is where `Option<u64>`
  arriving as `None` or an `int` is proven.
- `bindings/python/tests/test_memory.py:109` —
  `test_the_base_class_answers_for_a_store_that_keeps_no_file`; `:130`
  `test_the_bundled_store_is_a_memory_store`, the virtual subclassing; and `:153`
  `test_a_store_that_raises_reaches_the_caller_as_what_it_raised`, which is where
  the fallible round trip through `Catch`/`Raise` is proven.
- `bindings/python/tests/test_session.py:2380` —
  `test_a_python_store_is_opened_read_written_and_closed`; `:2438`
  `test_the_filter_runs_before_the_store_sees_a_note`; `:2484`
  `test_a_store_naming_no_file_is_confined_against_nothing`.
- `bindings/python/tests/test_session.py:2000` —
  `test_a_filter_sees_every_note_and_can_rewrite_or_drop_it`; `:2036`
  `test_a_filter_returning_none_keeps_the_note_out_of_the_file`; `:2064`
  `test_a_filter_that_raises_drops_the_note_and_says_so`, the gate failing
  closed; `:2102` `test_a_filter_that_is_not_callable_is_refused_at_construction`,
  which is refused at construction rather than at the first note an agent writes.
- `bindings/python/tests/test_session.py:2115` — `test_per_agent_memory` — and
  `:2164` `test_agent_without_memory_uses_session_memory`: `scope_for` observed
  through a live session.

## Open Gaps / Roadmap

- `FileMemory` rewrites through the loaded content on every append; a memory file
  that grows very large is re-read and re-written each time. A store that appends
  without rewriting is a store to install, not a change to the default.
- No locking beyond each store's own. Two processes pointed at one memory file
  through `FileMemory` will interleave writes.
- No size ceiling in the default store. A file large enough to fill the context
  window is a named error from `fit_conversation` (`session.rs:1681`) rather than
  a silent degradation, but `FileMemory` will not trim the file to avoid it:
  which notes are worth keeping is the caller's judgement. A store that bounds
  its own `read()` is the place that judgement belongs.
- No store is bundled beyond `FileMemory`. A summarising or retrieval-backed one
  is a plugin the framework can now carry without the session knowing.
