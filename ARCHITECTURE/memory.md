---
eatmycode_version: "1.1.0"
---

# Memory

## Goal

What the agents in a session remember, and where it is kept. It is the only
state that outlives a turn without being in the conversation, and what an agent
writes there goes into the next agent's prompt verbatim. M3 supplies metered,
per-scope maintenance and cleanup without paid provider work.

Memory is a slot. A session holds one `MemoryStore` and addresses it by *scope*
— a name whose meaning is the store's to decide. The default store, `FileMemory`,
reads a scope as a path and keeps free-form Markdown prose there rather than a
key-value structure. Two bundled stores bound what a scope may grow to, and they
differ in *who* does the bounding:

- `SummarizingMemory` keeps the most recent entries word for word and folds the
  rest into a running summary through one provider operation per overflowing
  scope during successful completion, scheduled as separate runtime steps.
  The framework decides, and the agents never see it happen.
- `CuratedMemory` holds a scope to a character ceiling and refuses the append
  that would cross it, telling the agent what is stored and to merge or remove
  something first. The agents decide, in the turn they were already taking, and
  no extra provider call is made.

A caller who wants a database or an embedding index installs a fourth and the
session is unchanged.

Three consequences follow from *verbatim*, and all three are the module's
business rather than its callers':

- Memory is a channel between agents. Text arriving from a model is filtered on
  the way in and quoted on the way out.
- Notes outlive the run that wrote them. A session resumed a week later reads
  week-old notes, so the scope's age travels with its content into the prompt.
- A store is not trusted to be a file. `path()` is how one that *is* opts into
  the workspace confinement the session file goes through.

The module does not own scope selection, the two write paths, or the memory
tools — those are [session.md](session.md)'s — nor the scheduling and metering
of maintenance, which is [run.md](run.md)'s.

## Status

`done`. `cargo test -p kerness --lib memory` passes 40 tests and
`cargo test -p kerness --lib usage` passes 6; the runtime owner
`memory_maintenance_is_stepped_metered_and_skipped_during_cleanup` passes in
`crates/kerness/tests/session_run.rs`; `bindings/python/tests/test_memory.py`
passes 17 and `bindings/python/tests/test_session.py` passes 122, including
the Python-store lifecycle and filter cases.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the store trait, the three bundled stores, the file primitive, the filter trait, and the shared file helpers |
| `crates/kerness/src/session.rs` | `Memories` (`crates/kerness/src/session.rs:230`), `remember` (`:271`), `revise_memory` (`:300`), `store_for` (`:323`), and the three memory tools in `default_tools` (`:2017`) |
| `crates/kerness/src/session/run.rs` | per-scope maintenance scheduling (`crates/kerness/src/session/run.rs:1148`), `advance_closing` (`:1164`), and guarded cleanup (`:1216`) |
| `crates/kerness/src/usage.rs` | `observe_provider_call` (`crates/kerness/src/usage.rs:603`) and `without_provider_calls` (`:522`), the two guards a store's provider use passes through |
| `bindings/python/src/memory.rs` | `PyStore`, a Python store seen as one; the three bundled stores as pyclasses; `PySessionMemory` |
| `bindings/python/src/session.rs` | `PyFilter` (`bindings/python/src/session.rs:179`) and `bind_memory_filter` (`:209`) |
| `bindings/python/src/types.rs` | `PyMemory` (`bindings/python/src/types.rs:969`), the file primitive on its own |
| `bindings/python/kerness/memory.py` | the `MemoryStore` ABC and the re-exports |

## Language and Conventions

Rust crate module, one PyO3 binding module plus two crossings in the session
binding, and a Python module that declares an ABC. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply; local facts:

- `memory.rs` imports `error`, `logging`, `provider`, and `usage`, and nothing
  above them; it never imports `session`. Every store method that does IO
  returns `Result`, and the two infallible metadata methods (`age`, `path`)
  and `budget` return `Option`.
- Each bundled store keeps a `Mutex<HashMap<..>>` and reaches it through a
  private `with` closure (`crates/kerness/src/memory.rs:307`, `:785`); the
  guard is recovered from poisoning with `unwrap_or_else(|err| err.into_inner())`
  rather than `expect`, because a store outlives a tool handler that unwound.
  `Mutex` rather than `RwLock` because a read of an unloaded scope loads it
  (`:292`).
- `REVISE_UNSUPPORTED` (`crates/kerness/src/memory.rs:277`) is a `pub const`
  precisely so
  `bindings/python/kerness/memory.py:131` raises the same text; a message
  spelled in two languages drifts.
- `MemoryStore` is a Python `ABC` (`bindings/python/kerness/memory.py:31`)
  because it is what callers subclass and an extension cannot declare one;
  the three bundled stores are `frozen, subclass` pyclasses registered as
  virtual subclasses (`:137`), so `isinstance` holds without inheritance.
- Unit tests are inline: `crate::testing::TempDir`
  (`crates/kerness/src/memory.rs:969`), an `Ephemeral` store that exercises
  every trait default (`:1088`), and a `StubProvider` built on `ProviderBase`
  (`:1134`) that records what it was asked. The
  Python tests use the conftest `MockProvider` and `Test<Store>` classes.

## Design and Invariants

### Decomposition and dependency direction

`MemoryStore` (`crates/kerness/src/memory.rs:151`) is the slot; `MemoryFilter`
(`:126`) is the gate in front of it. Three implementations sit beside the trait,
sharing three private helpers — `days_since_write` (`:70`), `scope_file`
(`:89`), `write_creating_parent` (`:105`) — and `Memory` (`:347`) is the file
primitive `FileMemory` is built out of. Above the module, [session.md](session.md)
owns the two write paths and the scope map, [run.md](run.md) owns when
maintenance and cleanup happen, and [prompting.md](prompting.md) owns how a
scope is framed in a prompt. The `usage` guards a store's provider call passes
through belong to [provider.md](provider.md).

### Scope, and why it is a string

`SessionConfig::memory` and `Agent::memory` are strings the session never
parses. `FileMemory` reads one as a path (`crates/kerness/src/memory.rs:340`);
another store reads it as a
key, a collection name, or a namespace. Nothing above `MemoryStore` assumes
memory is a file, which is what lets a store be replaced without the session,
the prompt assembler, or the memory tools changing.

The session takes the store and the scope together, under one lock, and then
calls the store with the lock released — `store_for`
(`crates/kerness/src/session.rs:323`). A store written in Python can run
arbitrary code, including code that re-enters the session; holding the
session's lock across that call is how it would deadlock.

### SummarizingMemory

The second bundled store, and the reason the slot is worth having: notes that
only ever grow eventually cost more of every prompt than they are worth. It
keeps one JSON file per scope under a root (`Scope`,
`crates/kerness/src/memory.rs:486`), holding a running
summary and the entries written since that summary was last rewritten. `read`
renders the summary — labelled `CONSOLIDATED_PREFIX` (`:447`), so an agent can
tell a framework-written recap from a note somebody wrote — and then the
entries (`:526`). `append` writes through on every note (`:634`), so a crash
mid-run loses nothing that was committed.

The run asks `maintenance_scopes()` (`crates/kerness/src/memory.rs:666`) for
sorted overflowing scopes
after a successful result (`crates/kerness/src/session/run.rs:1150`).
`maintain_scope(scope)` (`crates/kerness/src/memory.rs:681`) consolidates exactly one scope per runtime
step: one logical provider operation carrying the running summary and entries
beyond `with_keep(entries)` (`:563`). The call goes through
`observe_provider_call` (`:602`), so it participates in
[provider.md](provider.md)'s run ledger and budget checks, including default
retries or an explicitly opaque custom override. A cancelled, failed, or
abandoned run skips this paid maintenance and keeps its written notes.
Standalone `close()` (`:659`) drives the same per-scope methods to completion.

Cleanup calls `close_run()` (`crates/kerness/src/memory.rs:225`). Its default
delegates to `close()`; `SummarizingMemory` overrides it with no work (`:704`)
because each append and
completed consolidation is already saved. Custom cleanup must only flush and
release resources. The engine rejects observed framework provider calls
during cleanup, even without an accounting scope, and does not charge those
refused calls (`without_provider_calls`, `crates/kerness/src/usage.rs:522`,
applied at `crates/kerness/src/session/run.rs:1223` and `:295`). The guard
restores on unwind and reports an attempted forbidden call even if a callback
catches it. Arbitrary custom I/O or provider overrides that bypass framework
dispatch cannot be preempted or metered; those remain the store author's
responsibility.

The scope list and overflow are read under the store mutex, which is released
before calling a provider (`crates/kerness/src/memory.rs:681`). A provider can
read memory during its callback; notes appended during consolidation remain
after the summarized prefix (`:695`).

Two decisions are worth naming:

- **The provider is required at construction**
  (`crates/kerness/src/memory.rs:543`). A store built
  without one would keep every entry forever, which is what `FileMemory`
  already does, and it would do it silently.
- **An ordinary provider failure preserves the notes**
  (`crates/kerness/src/memory.rs:618`). The call
  returns `None` and the scope is left exactly as its agents wrote it. A run
  budget refusal remains terminal even when it interrupts consolidation
  retries (`crates/kerness/src/session/run.rs:1174`). This is
  [compaction.md](compaction.md)'s rule inverted: there, a failed summary
  means keeping turns that would have been dropped; here it means keeping
  notes that would have been rewritten. Both preserve what was actually
  written, and losing a run's notes to a network error is the worse of the
  two outcomes by a distance.

### CuratedMemory

The third bundled store, and the other answer to the same problem: a scope is
held to `budget()` characters — `DEFAULT_MEMORY_BUDGET`
(`crates/kerness/src/memory.rs:715`) is 2,200,
roughly 550 tokens at [compaction.md](compaction.md)'s `CHARS_PER_TOKEN` — and
the agents are the ones who keep it under. One Markdown file per scope under a
root, entries joined by `ENTRY_SEPARATOR` (`:724`) on lines of their own, so a
scope stays a file somebody can read and hand-edit.

Four decisions carry the design:

- **It does not compact.** An append that would cross the ceiling is an
  `Error::Value` (`full`, `crates/kerness/src/memory.rs:837`) carrying the figure it would have reached
  and the entries as they stand, telling the writer to merge or remove and
  write again. The agent is mid-turn and has the tool to do it, and the
  alternative — dropping the oldest note to make room — discards the caller's
  material on a guess about which note mattered least.
- **An entry is addressed by a fragment of itself.** `revise`
  (`crates/kerness/src/memory.rs:921`) takes
  any substring appearing in exactly one entry; `locate` (`:854`) refuses a
  fragment matching none or several and names which, because rewriting a
  guess is the one failure the writer cannot detect. The replacement replaces
  the whole entry, not the fragment, so a revision is never a blind splice.
- **`read` leads with the usage line** (`crates/kerness/src/memory.rs:885`)
  — characters used, the
  ceiling, and the entry count — because an agent that cannot see how full
  the scope is cannot be asked to make room in it. An empty scope reads as
  the empty string, so `memory_block` renders nothing at all rather than
  `0 of 2,200`.
- **An exact duplicate is accepted and not stored twice**
  (`crates/kerness/src/memory.rs:900`). A model
  re-writing a note it already wrote has made no mistake worth an error, and
  spending the ceiling on a second copy is the outcome nobody wants.

Answering `budget()` (`crates/kerness/src/memory.rs:961`) is also what makes
the session offer the `edit_memory` tool
(`crates/kerness/src/session.rs:2160`). The gate is deliberate: a store that
keeps notes append-only takes the trait's `revise` default, which refuses
(`crates/kerness/src/memory.rs:265`), and advertising a tool whose every call would
be refused is worse than not offering it.

### A scope is a key, not a path

Both stores that keep a root put every scope through `scope_file`
(`crates/kerness/src/memory.rs:89`),
which writes every byte outside `[A-Za-z0-9_-]` as `%XX`. The encoding is
reversible — so two scopes never collide on one file — and leaves no separator
and no `.` in the name, so a scope reading like `../../elsewhere` names a file
*under* the root rather than one outside it. Both stores still answer
`path(scope)`, so whatever they name is confined by the workspace as well; the
encoding is what makes them correct on their own rather than only correct
because something above them checked. Enforced by
`a_scope_is_a_key_and_never_a_path_out_of_the_root` (`:1360`) and its curated
twin (`:1599`).

### Age, read from the filesystem

All three bundled stores read the mtime through `days_since_write`
(`crates/kerness/src/memory.rs:70`)
rather than parsing the content, because none imposes a format the content
must carry a timestamp in and a timestamp parsed out of prose would be such a
format. A clock that has gone backwards since the write reads as `0` rather
than as an error: staleness is advisory, and no caveat is the better answer
when the age is not credible.

`None` and `Some(0)` are distinct and both reach the prompt.
[prompting.md](prompting.md)'s `memory_block`
(`crates/kerness/src/prompting.rs:79`) takes the age and renders a caveat past
`MEMORY_STALE_AFTER_DAYS`; a scope with no file yet holds only notes written
this run, which are as fresh as the run, so it carries none. A store with no
notion of a write time takes the trait's default
(`crates/kerness/src/memory.rs:181`) and is correct.

### The trust boundary

What an agent writes lands inside another agent's *system prompt*, which is
the position a session's own instructions occupy. Two separate mechanisms
answer that, at the two ends:

- On the way in, `MemoryFilter` (`crates/kerness/src/memory.rs:126`). It is
  applied in `remember`
  (`crates/kerness/src/session.rs:271`), a free function both write paths call
  — the `write_memory` tool and the `@MEMORY:` marker pass — so a caller who
  installs a filter cannot have it cover one and miss the other. A dropped
  note is reported to the writer as *not saved*, without saying which rule
  refused it: a specific rejection teaches a model how to word the next
  attempt.
- On the way out, `MEMORY_CAVEAT` and the `MEMORY_BEGIN`/`MEMORY_END` fence in
  [prompting.md](prompting.md). The block says plainly that it is recorded
  material, not instruction.

A revision is model output landing in the same place, so it takes the same
route: `revise_memory` (`crates/kerness/src/session.rs:300`) is `remember`'s
counterpart and the only path `edit_memory` has into a store. The replacement
text goes through the filter exactly as an appended note does, and a filter
that drops it changes nothing. A *removal* — an empty replacement — is
deliberately not filtered: the filter's contract is the text to store, and a
removal stores none.

The filter runs *before* the store sees a note, and the store is reached only
through `remember` and `revise_memory`. That ordering is the whole reason a
third-party store is safe to install: it cannot be a route around a filter
the caller put in. Enforced by `test_the_filter_runs_before_the_store_sees_a_note`
(`bindings/python/tests/test_session.py:2586`).

The framework ships no filter implementation. What counts as a secret, and
what a session is willing to persist, are the caller's to define; a redactor
guessing at it here would be wrong in both directions and wrong silently.

Two things are deliberately outside the boundary. A caller writing through
`Memory` or `session.memory` directly is not filtered
(`bindings/python/src/memory.rs:406` onward), because the caller is not the
untrusted party — and neither is the session's own closing `## Session Result`
block, which the framework composes rather than a model. From Python, a filter
that raises drops the note and logs a warning
(`bindings/python/src/session.rs:190`): the trait has no error path, and the
safe reading of a filter that could not decide is that the note stays out —
but silence would make that indistinguishable from a deliberate `None`.

### Failing early, and failing soft

The two directions are deliberate and different.

`open()` (`crates/kerness/src/memory.rs:171`) is fallible and runs before the
first turn
(`crates/kerness/src/session.rs:1077`), so an unreachable store costs nothing.
`append()`, `revise()`, and `close_run()` are fallible and propagate, because
a note that was not stored is a result the caller must see — and for `revise`
the refusal *is* the message the agent acts on.

`memory_text` (`crates/kerness/src/session.rs:398`) is the exception: it logs
and yields an empty block rather than failing the run. `PromptAssembler` takes
an infallible `Fn(&Agent) -> String`, and a read that failed *this late* would
discard provider calls already paid for — every scope was opened successfully
before the first turn, so a failure here arose mid-run.

The infallible trait methods take the same shape at the binding. `PyStore`'s
`optional` (`bindings/python/src/memory.rs:47`) logs and answers `None` when a
Python `age`, `path`, or `budget` raises: those return `Option`, not `Result`,
and the honest reading of a store that cannot name its file is a store that
names none — which is what a store keeping nothing on disk answers anyway. A
store that cannot name a ceiling is read the same way, and simply is not
offered `edit_memory`. An invalid or failed Python `maintenance_scopes`
likewise logs and yields an empty list (`:131`), because the Rust trait's
listing has no error return.

### Invariants a change must preserve

- A store is reached by model output only through `remember` and
  `revise_memory`; both filter first. No test enforces that no third path
  exists; the two functions being private free functions in `session.rs` is
  the structural guarantee.
- `maintenance_scopes()` must not call a provider
  (`crates/kerness/src/memory.rs:209`); `close_run()` must not start provider
  work (`:225`) and is refused if it tries (`crates/kerness/src/usage.rs:799`).
- Every scope opens before the first provider turn, and opened resources are
  closed exactly once on completion, terminal failure, cancellation, and
  abandonment. Enforced by `an_installed_store_is_opened_read_written_and_closed`
  (`crates/kerness/src/session.rs:3422`) and
  `memory_maintenance_is_stepped_metered_and_skipped_during_cleanup`
  (`crates/kerness/tests/session_run.rs:643`).
- A scope under a root never names a file outside it
  (`crates/kerness/src/memory.rs:89`).
- A failed consolidation or a refused revision leaves the scope exactly as
  written (`crates/kerness/src/memory.rs:618`, `:921`).
- The maintenance cursor is checkpointed only after the budget check
  (`crates/kerness/src/session/run.rs:1174`), and a restored checkpoint with
  an inconsistent cursor is refused (`:1302`).

### The Python surface

`MemoryStore` is an ABC in `bindings/python/kerness/memory.py` for the reason
`Channel` is one ([bindings.md](bindings.md)): it is what callers subclass, and
an extension cannot declare an abstract base class. All three bundled stores
are registered against it rather than inheriting from it, so `isinstance`
holds without an extension type subclassing a Python ABC.

`budget` and `revise` are concrete on the ABC
(`bindings/python/kerness/memory.py:105`, `:116`), and `revise` raises
`ValueError` rather than returning. The exception class is the choice that
matters: `ValueError` crosses back as `Error::Value`, which is what the Rust
default returns, so a Python store that does not override it and a Rust store
that does not are indistinguishable to the session.

`SummarizingMemory(root, provider, model, keep=DEFAULT_KEEP_ENTRIES)`
(`bindings/python/src/memory.rs:262`) binds its provider exactly the way an
agent's is bound, so a `Provider` subclass written in Python is what does the
summarising when one is passed. `CuratedMemory(root, budget=DEFAULT_MEMORY_BUDGET)`
(`bindings/python/src/memory.rs:336`) needs no provider and forwards the
ordinary store methods.

The ABC, all three native store wrappers, and `PyStore` expose
`maintenance_scopes`, `maintain_scope`, and `close_run`. Custom Python stores
can therefore declare scopes for Rust to schedule and meter. `PyStore::close_run`
(`bindings/python/src/memory.rs:147`) calls `close_run` when the object has one and `close` otherwise, so an
existing store that only flushes resources keeps working; stores doing paid
work move that work into `maintain_scope`. The `FileMemory` and
`CuratedMemory` pyclasses forward `close_run` to the Python-visible `close`
(`:244`, `:385`) so a subclass overriding `close` still runs during cleanup;
`SummarizingMemory` has a separate no-work `close_run` (`:321`), because its
standalone `close` performs paid consolidation. Scheduling and budget
decisions stay in Rust.

`bind_memory_store` (`bindings/python/src/memory.rs:165`) hands the session the `Arc<dyn MemoryStore>`
inside a bundled pyclass on the *exact* type only (`:172`); a subclass
overriding `read` is a caller's store that happens to inherit, and the
shortcut past it would call the base the subclass exists to wrap. Anything
else becomes a `PyStore` (`:181`).

`session.memory` returns `PySessionMemory`
(`bindings/python/src/memory.rs:395`), not a `Memory`. The
distinction is observable — `session.memory.read()` after `run()` has to show
what the run wrote, which a snapshot taken at construction would not — and it
is honest about the store: `path` and `age` are `None` when the store keeps no
file, and there is no `write()`, because replacing everything is not something
the trait offers.

`PromptAssembler`'s `memory_for` callback is live for the same reason: it is
called per turn, so two agents pointed at one scope both see each other's
writes, and a scope written mid-run stops being stale without the session
rebuilding anything.

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:151` — `MemoryStore` — `read` and `append`
  required; `open`, `age`, `path`, `close`, `maintenance_scopes`,
  `maintain_scope`, `close_run`, `budget`, and `revise` defaulted. Every
  method takes `&self`; one store serves every agent behind an `Arc`.
- `crates/kerness/src/memory.rs:126` — `MemoryFilter` — `filter(note, actor)
  -> Option<String>`: the text to store, or `None` to drop the note. No error
  path.
- `crates/kerness/src/memory.rs:287` — `FileMemory` — the default: the scope
  is the path, one lazily loaded `Memory` per scope, no ceiling, `path` is
  `Some(scope)`.
- `crates/kerness/src/memory.rs:347` — `Memory` — the file primitive:
  `load` (`:383`), `read`, `append_entry` (`:410`), `write` (`:425`), and
  `age`; usable alone by a caller who wants one file and no session.
- `crates/kerness/src/memory.rs:475` — `SummarizingMemory` — `new(root,
  provider, model)` (`:543`), `with_keep` (`:563`); `maintenance_scopes`
  (`:666`) lists overflowing scopes sorted, `maintain_scope` (`:681`) spends
  one provider operation on one of them, `close_run` (`:704`) does nothing.
- `crates/kerness/src/memory.rs:749` — `CuratedMemory` — `new(root)` (`:761`),
  `with_budget` (`:774`); `append` (`:900`) refuses past the ceiling with
  `Error::Value`, `revise` (`:921`) addresses one entry by fragment, `budget`
  (`:961`) answers `Some`.
- `crates/kerness/src/memory.rs:277` — `REVISE_UNSUPPORTED` — the refusal the
  trait default and the Python ABC both raise.
- `crates/kerness/src/session.rs:230` — `Memories` — the store plus the
  session scope and per-agent scopes; `scope_for` (`:238`) and `scopes`
  (`:249`) are how a session addresses it; `Session::memories` (`:703`) is
  the live handle.
- `crates/kerness/src/session.rs:271` — `remember`; `:300` `revise_memory` —
  the only two paths model output takes into a store; return whether the note
  was stored, propagate the store's error.
- `bindings/python/src/memory.rs:165` — `bind_memory_store(object)` — a
  bundled pyclass passes its inner `Arc` through on exact type; anything else
  is wrapped as `PyStore` (`:32`).

## Interactions

- Rendered into a system prompt by [prompting.md](prompting.md)'s
  `memory_block` (`crates/kerness/src/prompting.rs:79`), which also owns the
  caveat and the staleness line; the shared value is the scope's `read()`
  text and `age()`.
- Held per session by [session.md](session.md)'s `Memories`, which also holds
  the configured `memory_filter` (`crates/kerness/src/session.rs:153`);
  every scope is opened at preparation (`:1077`).
- Confined by [access.md](access.md): whatever `path(scope)` names is checked
  against the workspace at `Session::new` (`crates/kerness/src/session.rs:576`)
  and, for a per-agent scope, at preparation. A store answering `None` is
  checked against nothing
  (`test_a_store_naming_no_file_is_confined_against_nothing`,
  `bindings/python/tests/test_session.py:2632`).
- Read and written through the `read_memory`, `write_memory`, and
  `edit_memory` tools registered in `default_tools`
  (`crates/kerness/src/session.rs:2104`, `:2126`, `:2163`); `write_memory`
  only for a writing session, `edit_memory` only when the store answers
  `budget()`. See [toolkit.md](toolkit.md) for dispatch.
- Memory markers in a reply are extracted by [utils.md](utils.md)'s
  `parse_memory_markers` (`crates/kerness/src/utils.rs:122`) and never reach
  the transcript (`memory_markers_never_reach_the_transcript_or_the_channel`,
  `crates/kerness/tests/session_run.rs:974`).
- Counted as prompt overhead by [compaction.md](compaction.md): memory is
  part of the system message, so it narrows what the conversation may use.
- Successful [run.md](run.md) completion schedules one maintenance scope per
  step (`crates/kerness/src/session/run.rs:1148`, `:1164`) and closes the
  store once under the no-provider guard (`:1216`);
  [provider.md](provider.md)'s `observe_provider_call` and
  `without_provider_calls` (`crates/kerness/src/usage.rs:603`, `:522`) are
  the two guards a store's provider use passes through.
- The Python `memory_store=` and `memory_filter=` keywords cross at
  `bindings/python/src/session.rs:393` and `:411`; `Session.memory` returns
  `PySessionMemory` (`:436`).

## How to Test

```sh
cargo test -p kerness --lib memory                                  # pass = 40 passed
cargo test -p kerness --lib usage                                   # pass = 6 passed
cargo test -p kerness --test session_run memory_maintenance         # pass = 1 passed
.venv/bin/python -m pytest bindings/python/tests/test_memory.py -q  # pass = 17 passed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q # pass = 122 passed
```

Rebuild the Python extension (`cd bindings/python && ../../.venv/bin/maturin
develop`) before running the Python suites after a Rust change.

- The file primitive and the default store:
  `loading_an_absent_file_reads_empty_and_creates_nothing`
  (`crates/kerness/src/memory.rs:972`),
  `entries_are_separated_by_a_blank_line_and_nothing_else_is_added` (`:983`),
  `the_default_store_keeps_one_file_per_scope` (`:1045`),
  `the_default_store_reports_its_path_and_the_age_of_the_file` (`:1076`);
  from Python, `TestMemory` (`bindings/python/tests/test_memory.py:23`
  onward).
- Every trait default, from a store that keeps nothing:
  `a_store_writing_no_file_answers_the_defaults_and_leaves_no_trace`
  (`crates/kerness/src/memory.rs:1105`) and `test_the_base_class_answers_for_a_store_that_keeps_no_file`
  (`bindings/python/tests/test_memory.py:119`).
- SummarizingMemory: `entries_read_back_verbatim_until_something_consolidates_them`
  (`crates/kerness/src/memory.rs:1197`), `closing_folds_everything_past_the_kept_entries_into_one_summary`
  (`:1214`) — standalone consolidation, sorted scopes, one operation per
  maintenance call, budget refusal, and cleanup without paid calls —
  `a_second_consolidation_is_given_the_first_one_to_build_on` (`:1308`),
  `a_failed_consolidation_keeps_the_notes_as_they_were_written` (`:1328`),
  `a_scope_is_a_key_and_never_a_path_out_of_the_root` (`:1360`); from
  Python, `TestSummarizingMemory` (`bindings/python/tests/test_memory.py:217`
  onward), including a subclassed store handed to a session (`:262`).
- CuratedMemory: `a_store_with_no_ceiling_refuses_to_revise_and_says_so`
  (`crates/kerness/src/memory.rs:1395`), `entries_read_back_behind_a_line_saying_how_full_the_scope_is`
  (`:1410`), `a_note_already_stored_word_for_word_is_accepted_and_not_stored_twice`
  (`:1435`), `an_append_past_the_ceiling_is_refused_and_says_what_is_stored`
  (`:1448`), `revising_replaces_the_whole_entry_a_fragment_addresses`
  (`:1471`), `revising_to_nothing_removes_the_entry` (`:1492`),
  `a_fragment_matching_none_or_several_changes_nothing_and_names_which`
  (`:1507`), `a_revision_past_the_ceiling_is_refused_and_the_entry_survives`
  (`:1542`), `a_hand_edited_file_loads_as_the_entries_it_visibly_holds`
  (`:1577`); from Python, `TestCuratedMemory`
  (`bindings/python/tests/test_memory.py:292` onward), including
  `test_a_store_written_in_python_is_asked_for_its_ceiling` (`:325`).
- The cleanup guard: `scopes_restore_on_return_and_unwind_without_cross_run_attribution`
  (`crates/kerness/src/usage.rs:785`) covers absent collectors, nesting,
  unwind restoration, and uncharged refusals through
  `without_provider_calls`.
- The lifecycle through a session:
  `an_installed_store_is_opened_read_written_and_closed`
  (`crates/kerness/src/session.rs:3422`),
  `memory_maintenance_is_stepped_metered_and_skipped_during_cleanup`
  (`crates/kerness/tests/session_run.rs:643`) — successful, budget-limited,
  cancelled, and abandoned runs — and
  `test_a_python_store_is_opened_read_written_and_closed`
  (`bindings/python/tests/test_session.py:2492`).
- The trust boundary: `test_the_filter_runs_before_the_store_sees_a_note`
  (`bindings/python/tests/test_session.py:2586`); `TestMemoryMarkers`
  (`:2022`) and `TestMemoryTools` (`:2313`) cover the two write paths and
  `test_write_memory_is_offered_only_to_a_writing_session` (`:2345`).
- `test_a_store_that_raises_reaches_the_caller_as_what_it_raised`
  (`bindings/python/tests/test_memory.py:192`) — a Python store's exception
  crossing back through the framework conversion.
- Gap: `PyStore::revise` (`bindings/python/src/memory.rs:105`) is the one
  crossing no test drives; see Open Gaps.

## Review and Refactor Guide

- **Adding a bundled store** → implement `MemoryStore` in `memory.rs`, route
  file naming through `scope_file` (`crates/kerness/src/memory.rs:89`) and
  writes through `write_creating_parent` (`:105`), add a `frozen, subclass`
  pyclass in `bindings/python/src/memory.rs` with an exact-type arm in
  `bind_memory_store` (`bindings/python/src/memory.rs:172`) and a `MemoryStore.register` line in
  `bindings/python/kerness/memory.py:137`, and extend
  `test_the_bundled_stores_are_memory_stores`
  (`bindings/python/tests/test_memory.py:149`). Keep `path()` answering when
  the store writes a file, so the workspace confines it.
- **Adding a trait method** → give it a default
  (`crates/kerness/src/memory.rs:151` onward), forward it from `PyStore`
  (`bindings/python/src/memory.rs:82`) and all three pyclasses, and add it to
  the ABC; a method with an `Option`/`Vec` return goes through `optional`
  (`:47`) and logs rather than raises.
- **Changing when maintenance runs** → `finish` and `advance_closing`
  (`crates/kerness/src/session/run.rs:1148`, `:1164`), `ClosingState`
  (`:233`), the restore check (`:1302`), and the runtime owner test
  (`crates/kerness/tests/session_run.rs:643`). The cursor is checkpoint
  state.
- **Changing the filter contract** → `remember` and `revise_memory`
  (`crates/kerness/src/session.rs:271`, `:300`), `PyFilter`
  (`bindings/python/src/session.rs:179`), and the boundary test at
  `bindings/python/tests/test_session.py:2586`. Both write paths must keep
  calling the same function.
- **Changing a refusal message** → `full`
  (`crates/kerness/src/memory.rs:837`), `locate` (`:854`),
  `REVISE_UNSUPPORTED` (`:277`); the Python memory tests match on wording and
  `REVISE_UNSUPPORTED` is imported, not respelled, on the Python side.
- **Changing the memory tools** → `default_tools`
  (`crates/kerness/src/session.rs:2017`) and `TestMemoryTools`
  (`bindings/python/tests/test_session.py:2313`); the `edit_memory`
  description states the ceiling figure.
- Safe extension points: the defaulted trait methods; `with_keep` and
  `with_budget`; `MemoryFilter`, which the framework never implements.
- Forbidden coupling: `memory.rs` must not import `session`, `prompting`, or
  `access`; a store that reaches the session's lock is the deadlock
  `store_for` exists to prevent.
- Compatibility: `DEFAULT_KEEP_ENTRIES`, `DEFAULT_MEMORY_BUDGET`, and
  `ENTRY_SEPARATOR` are in the root's well-known constants table and asserted
  by `crates/kerness/tests/public_api.rs`; the Python constructor keyword
  names are public.

Improvement candidates (proposals, not accepted work):

- A scripted `edit_memory` tool call against a Python store would drive
  `PyStore::revise` and close the one untested crossing. Success check: a
  Python store's `revise` is called with the fragment and replacement the
  model sent.
- A `pyo3` `multiple-pymethods`-free way to share the forwarding block across
  the three pyclasses does not exist today; revisit if a fourth bundled store
  arrives.

## Open Gaps / Roadmap

- `FileMemory` caches each scope on first use and rewrites its whole file on
  every append. Large files therefore cost more to rewrite. A store that
  appends without rewriting can be installed when needed.
- No locking beyond each store's own. Two processes pointed at one memory file
  through `FileMemory` will interleave writes.
- No size ceiling in the default store. A file large enough to fill the
  context window is a named error from `fit_conversation` rather than a
  silent degradation, but `FileMemory` will not trim the file to avoid it:
  which notes are worth keeping is the caller's judgement. `SummarizingMemory`
  and `CuratedMemory` are where that judgement is made — each bounds its own
  `read()` by construction — and a caller who wants a bound takes one.
- The three bundled stores are three pyclasses forwarding one-line trait
  methods (`bindings/python/src/memory.rs`). A macro was considered and
  declined: pyo3 0.23 needs the `multiple-pymethods` feature to split a
  `#[pymethods]` block, and `#[pymethods]` will not expand a `macro_rules!`
  invocation inside the impl, so the only shape that works wraps the whole
  block — more machinery than the forwarding it saves. A fourth bundled store
  is the point at which that trade changes.
- `PyStore::revise` (`bindings/python/src/memory.rs:105`) is the one crossing
  in this module no test drives. Reaching it needs Rust to call `revise` on a
  store written in Python, which needs a session whose agent issues an
  `edit_memory` call — a scripted tool-call run, and a brittle one. The five
  directions either side of it are covered (`budget` by
  `test_a_store_written_in_python_is_asked_for_its_ceiling`, the rest by
  `test_a_python_store_is_opened_read_written_and_closed`), so what is
  untested is the forwarding, not the contract.
- **Retrieval by relevance is not expressible.** `MemoryStore::read(scope)`
  carries no query, so a store backed by an embedding index can only answer
  with the whole scope, and a retrieval store is exactly the shape the trait
  cannot hold. Adding a query is a contract change — every store gains a
  parameter, and the prompt assembler has to have something to ask about — so
  it is a decision rather than an omission, and it is not taken here.
- `SummarizingMemory` writes one filename per scope with every byte outside
  `[A-Za-z0-9_-]` expanded threefold. A scope long enough that the encoded
  name passes the filesystem's limit fails as a named `Error::Io` naming the
  path, rather than being truncated into a collision.
