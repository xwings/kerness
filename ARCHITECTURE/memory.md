# Memory

## Goal

What the agents in a session remember, and where it is kept. It is the only
state that outlives a turn without being in the conversation, and what an agent
writes there goes into the next agent's prompt verbatim. M3 adds metered,
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

## Status

`done` — M3 memory maintenance is scheduled per scope and metered; cleanup
starts no observed provider work. The listed Rust tests prove this contract
while preserving standalone store behavior.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the store trait, the three bundled stores, the file primitive, and the filter trait |
| `crates/kerness/src/session.rs` | `Memories` — the store plus the scope each agent addresses it by |
| `crates/kerness/src/session.rs` | `remember`, the one path model output takes into memory |
| `crates/kerness/src/session.rs` | `revise_memory`, the same path for a revision |
| `crates/kerness/src/session/run.rs` | per-scope maintenance scheduling and guarded cleanup |
| `bindings/python/src/memory.rs` | the boundary: a Python store seen as one, and the bundled store seen from Python |
| `bindings/python/src/session.rs` | `PyFilter`, a Python callable behind the filter trait |
| `bindings/python/src/types.rs` | `PyMemory`, the file primitive on its own |
| `bindings/python/kerness/memory.py` | the `MemoryStore` ABC and the re-exports |

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:151` — `MemoryStore` — required reads/appends and additive maintenance/cleanup defaults.
- `crates/kerness/src/memory.rs:209` — `maintenance_scopes` / `maintain_scope` — list pending scopes without paid work, then maintain one scope per step.
- `crates/kerness/src/memory.rs:225` — `close_run` — flush/release only, defaulting to legacy `close`.
- `crates/kerness/src/memory.rs:287` — `FileMemory` — one lazily loaded file per scope.
- `crates/kerness/src/memory.rs:347` — `Memory` — standalone file primitive, load/read/append/write and file age.
- `crates/kerness/src/memory.rs:475` — `SummarizingMemory` — retained entries plus model consolidation in separate maintenance steps.
- `crates/kerness/src/memory.rs:749` — `CuratedMemory` — bounded entries with explicit revise/remove operations.
- `crates/kerness/src/memory.rs:126` — `MemoryFilter` — caller policy for model-authored notes and revisions.
- `crates/kerness/src/session.rs:230` — `Memories` — session/agent scope selection and the shared store.
- `bindings/python/src/memory.rs:165` — `bind_memory_store` — Python adapter with direct Rust ownership for bundled stores.

### Scope, and why it is a string

`SessionConfig::memory` and `Agent::memory` are strings the session never
parses. `FileMemory` reads one as a path; another store reads it as a key, a
collection name, or a namespace. Nothing above `MemoryStore` assumes memory is a
file, which is what lets a store be replaced without the session, the prompt
assembler, or the two memory tools changing.

The session takes the store and the scope together, under one lock, and then
calls the store with the lock released — `store_for`. A store
written in Python can run arbitrary code, including code that re-enters the
session; holding the session's lock across that call is how it would deadlock.

### SummarizingMemory

The second bundled store, and the reason the slot is worth having: notes that
only ever grow eventually cost more of every prompt than they are worth. It
keeps one JSON file per scope under a root, holding a running summary and the
entries written since that summary was last rewritten. `read` renders the
summary — labelled `CONSOLIDATED_PREFIX`, so an agent can tell
a framework-written recap from a note somebody wrote — and then the entries.
`append` writes through on every note, so a crash mid-run loses nothing that was
committed.

The run asks `maintenance_scopes()` for sorted overflowing scopes after a
successful result. `maintain_scope(scope)` consolidates exactly one scope per
runtime step: one logical provider operation carrying the running summary and
entries beyond `with_keep(entries)`. The provider wrapper
participates in [provider.md](provider.md)'s run ledger and budget checks,
including default retries or an explicitly opaque custom override. A cancelled,
failed, or abandoned run skips this paid maintenance and keeps its written
notes. Standalone `close()` retains its prior behavior by driving the same
per-scope methods to completion.

Cleanup calls `close_run()`. Its default delegates to existing custom stores'
`close()`; `SummarizingMemory` overrides it with no work because each append and
completed consolidation is already saved. Custom cleanup must only flush and
release resources. The engine rejects observed framework provider calls during
cleanup, even without an accounting scope, and does not charge those refused
calls. The guard restores on unwind and reports an attempted forbidden call
even if a callback catches it. Arbitrary custom I/O or provider overrides that
bypass framework dispatch cannot be preempted or metered; those remain the
store author's responsibility.

The scope list and overflow are read under the store mutex, which is released
before calling a provider. A provider can read memory during its callback;
notes appended during consolidation remain after the summarized prefix.

Two decisions are worth naming:

- **The provider is required at construction**. A store built
  without one would keep every entry forever, which is what `FileMemory` already
  does, and it would do it silently.
- **An ordinary provider failure preserves the notes**. The call
  returns `None` and the scope is left exactly as its agents wrote it. A run
  budget refusal remains terminal even when it interrupts consolidation
  retries. This is
  [compaction.md](compaction.md)'s rule inverted: there, a failed summary means
  keeping turns that would have been dropped; here it means keeping notes that
  would have been rewritten. Both preserve what was actually written, and losing
  a run's notes to a network error is the worse of the two outcomes by a
  distance.

### CuratedMemory

The third bundled store, and the other answer to the same problem: a scope is
held to `budget()` characters — `DEFAULT_MEMORY_BUDGET` is
2,200, roughly 550 tokens at [compaction.md](compaction.md)'s `CHARS_PER_TOKEN`
— and the agents are the ones who keep it under. One
Markdown file per scope under a root, entries joined by `ENTRY_SEPARATOR`
on lines of their own, so a scope stays a file somebody can read and hand-edit.

Four decisions carry the design:

- **It does not compact.** An append that would cross the ceiling is an
  `Error::Value` carrying the figure it would have reached and
  the entries as they stand, telling the writer to merge or remove and write
  again. The agent is mid-turn and has the tool to do it, and the alternative —
  dropping the oldest note to make room — discards the caller's material on a
  guess about which note mattered least.
- **An entry is addressed by a fragment of itself.** `revise` takes any substring
  appearing in exactly one entry; `locate` refuses a fragment
  matching none or several and names which, because rewriting a guess is the one
  failure the writer cannot detect. The replacement replaces the whole entry, not
  the fragment, so a revision is never a blind splice.
- **`read` leads with the usage line** — characters used, the
  ceiling, and the entry count — because an agent that cannot see how full the
  scope is cannot be asked to make room in it. An empty scope reads as the empty
  string, so `memory_block` renders nothing at all rather than `0 of 2,200`.
- **An exact duplicate is accepted and not stored twice**. A
  model re-writing a note it already wrote has made no mistake worth an error,
  and spending the ceiling on a second copy is the outcome nobody wants.

Answering `budget()` is also what makes the session offer the `edit_memory` tool. The gate is deliberate: a store that keeps notes append-only
takes the trait's `revise` default, which refuses, and advertising a tool whose
every call would be refused is worse than not offering it.

### A scope is a key, not a path

Both stores that keep a root put every scope through `scope_file`, which writes every byte outside `[A-Za-z0-9_-]` as `%XX`. The
encoding is reversible — so two scopes never collide on one file — and leaves no
separator and no `.` in the name, so a scope reading like `../../elsewhere` names
a file *under* the root rather than one outside it. Both stores still answer
`path(scope)`, so whatever they name is confined by the workspace as well; the
encoding is what makes them correct on their own rather than only correct because
something above them checked.

### Age, read from the filesystem

All three bundled stores read the mtime through `days_since_write`
rather than parsing the content, because none imposes a format
the content must carry a timestamp in and a timestamp parsed out of prose would
be such a format. A clock that has gone backwards since the write reads as `0`
rather than as an error: staleness is advisory, and no caveat is the better
answer when the age is not credible.

`None` and `Some(0)` are distinct and both reach the prompt.
[prompting.md](prompting.md)'s `memory_block` takes the age and renders a caveat
past `MEMORY_STALE_AFTER_DAYS`; a scope with no file yet holds only notes written
this run, which are as fresh as the run, so it carries none. A store with no
notion of a write time takes the trait's default and is correct.

### The trust boundary

What an agent writes lands inside another agent's *system prompt*, which is the
position a session's own instructions occupy. Two separate mechanisms answer
that, at the two ends:

- On the way in, `MemoryFilter`. It is applied in `remember`, a free function both write paths call — the `write_memory`
  tool  and the `@MEMORY:` marker pass —
  so a caller who installs a filter cannot have it cover one and miss the other.
  A dropped note is reported to the writer as *not saved*, without saying which
  rule refused it: a specific rejection teaches a model how to word the next
  attempt.
- On the way out, `MEMORY_CAVEAT` and the `MEMORY_BEGIN`/`MEMORY_END` fence in
  [prompting.md](prompting.md). The block says plainly that it is recorded
  material, not instruction.

A revision is model output landing in the same place, so it takes the same route:
`revise_memory` is `remember`'s counterpart and the only path
`edit_memory` has into a store. The replacement text goes through the filter
exactly as an appended note does, and a filter that drops it changes nothing. A
*removal* — an empty replacement — is deliberately not filtered: the filter's
contract is the text to store, and a removal stores none.

The filter runs *before* the store sees a note, and the store is reached only
through `remember` and `revise_memory`. That ordering is the whole reason a
third-party store is safe to install: it cannot be a route around a filter the
caller put in.

The framework ships no filter implementation. What counts as a secret, and what
a session is willing to persist, are the caller's to define; a redactor guessing
at it here would be wrong in both directions and wrong silently.

Two things are deliberately outside the boundary. A caller writing through
`Memory` or `session.memory` directly is not filtered, because the caller is not
the untrusted party — and neither is the session's own closing
`## Session Result` block, which the framework composes
rather than a model. From Python, a filter that raises drops the note and logs a
warning: the trait has no error path, and
the safe reading of a filter that could not decide is that the note stays out —
but silence would make that indistinguishable from a deliberate `None`.

### Failing early, and failing soft

The two directions are deliberate and different.

`open()` is fallible and runs before the first turn, so an unreachable store
costs nothing. `append()`, `revise()`, and `close_run()` are
fallible and propagate, because a note that was not stored is a result the caller
must see — and for `revise` the refusal *is* the message the agent acts on.

`memory_text` is the exception: it logs and yields an empty
block rather than failing the run. `PromptAssembler` takes an infallible
`Fn(&Agent) -> String`, and a read that failed *this late* would discard provider
calls already paid for — every scope was opened successfully before the first
turn, so a failure here arose mid-run.

The infallible trait methods take the same shape at the binding.
`PyStore`'s `optional` logs and answers
`None` when a Python `age`, `path`, or `budget` raises: those return `Option`,
not `Result`, and the honest reading of a store that cannot name its file is a
store that names none — which is what a store keeping nothing on disk answers
anyway. A store that cannot name a ceiling is read the same way, and simply is
not offered `edit_memory`.
An invalid or failed Python `maintenance_scopes` likewise logs and yields an
empty list, because the Rust trait's listing has no error return.

### The Python surface

`MemoryStore` is an ABC in `bindings/python/kerness/memory.py` for the reason
`Channel` is one ([bindings.md](bindings.md)): it is what callers subclass, and
an extension cannot declare an abstract base class. All three bundled stores are
registered against it rather than inheriting from it, so
`isinstance` holds without an extension type subclassing a Python ABC.

`budget` and `revise` are concrete on the ABC, and
`revise` raises `ValueError` rather than returning. The exception class is the
choice that matters: `ValueError` crosses back as `Error::Value`, which is what
the Rust default returns, so a Python store that does not override it and a Rust
store that does not are indistinguishable to the session. The message is
`REVISE_UNSUPPORTED`, imported rather than respelled: the two
halves of one default spelled out in two languages drift silently, and this one
is text a caller reads.

`SummarizingMemory(root, provider, model, keep=DEFAULT_KEEP_ENTRIES)`
binds its provider exactly the way an agent's is bound, so a `Provider` subclass
written in Python is what does the summarising when one is passed.
`CuratedMemory(root, budget=DEFAULT_MEMORY_BUDGET)`
needs no provider and forwards the ordinary store methods.

The ABC, all three native store wrappers, and the Python-to-Rust adapter expose
`maintenance_scopes`, `maintain_scope`, and `close_run`. Custom Python stores can
therefore declare scopes for Rust to schedule and meter. The default
`close_run` delegates to legacy `close` for stores that only flush resources;
stores doing paid work move that work into `maintain_scope`. Subclasses of native
stores retain the inherited maintenance and cleanup methods when adapted through
`PyStore`. Scheduling and budget decisions stay in Rust.

`FileMemory` and `CuratedMemory` preserve Python subclasses' legacy `close`
overrides during run cleanup. `SummarizingMemory` has a separate no-work
`close_run`, because its standalone `close` performs paid consolidation. Its
Python subclasses override `close_run` to add run cleanup and use
`maintain_scope` for paid work.

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
- Held per session by [session.md](session.md)'s `Memories`,
  which also holds the configured `memory_filter`.
- Confined by [access.md](access.md): whatever `path(scope)` names is checked
  against the workspace at `Session::new` and, for a per-agent
  scope, at the top of `run()`.
- Read and written through the `read_memory`, `write_memory`, and `edit_memory`
  tools registered by [toolkit.md](toolkit.md); the last is offered only when the
  store answers `budget()`.
- Memory markers in a reply are extracted by [utils.md](utils.md)'s
  `parse_memory_markers`.
- Counted as prompt overhead by [compaction.md](compaction.md): memory is part of
  the system message, so it narrows what the conversation may use.
- Successful [run.md](run.md) completion schedules individual memory
  maintenance steps; [provider.md](provider.md) accounts for their model calls.

## How to Test

```sh
cargo test -p kerness --lib memory
cargo test -p kerness --lib usage
cargo test -p kerness --test session_run memory_maintenance
.venv/bin/python -m pytest bindings/python/tests/test_memory.py -q
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q
```

Pass means exit code 0 and no failed tests. Rebuild the Python extension before
running its tests after a Rust change.

The core-upgrade verification passed all 40 memory-filtered Rust tests, the
usage and runtime maintenance checks, and 21 rebuilt Python memory/custom-store
lifecycle cases, including native subclasses and metered custom maintenance.
The complete workspace gate is recorded in [testing.md](testing.md).

- `crates/kerness/src/memory.rs:1214` — Standalone consolidation, sorted scopes, one operation per maintenance call, budget refusal, and cleanup without paid calls.
- `crates/kerness/src/usage.rs:785` — Cleanup guards cover absent collectors, nesting, unwind restoration, and uncharged refusals.
- `crates/kerness/tests/session_run.rs:643` — Successful, budget-limited, cancelled, and abandoned runtime maintenance.
- `crates/kerness/src/memory.rs:1328` — Ordinary model failure preserves written notes.
- `crates/kerness/src/memory.rs:1542` — Curated-memory limit failures leave the original entry intact.

## Open Gaps / Roadmap

- `FileMemory` caches each scope on first use and rewrites its whole file on
  every append. Large files therefore cost more to rewrite. A store that appends
  without rewriting can be installed when needed.
- No locking beyond each store's own. Two processes pointed at one memory file
  through `FileMemory` will interleave writes.
- No size ceiling in the default store. A file large enough to fill the context
  window is a named error from `fit_conversation` rather than
  a silent degradation, but `FileMemory` will not trim the file to avoid it:
  which notes are worth keeping is the caller's judgement. `SummarizingMemory`
  and `CuratedMemory` are where that judgement is made — each bounds its own
  `read()` by construction — and a caller who wants a bound takes one.
- The three bundled stores are three pyclasses forwarding one-line
  trait methods (`bindings/python/src/memory.rs`). A macro
  was considered and declined: pyo3 0.23 needs the `multiple-pymethods` feature
  to split a `#[pymethods]` block, and `#[pymethods]` will not expand a
  `macro_rules!` invocation inside the impl, so the only shape that works wraps
  the whole block — more machinery than the forwarding it saves. A fourth
  bundled store is the point at which that trade changes.
- `PyStore::revise` is the one crossing in
  this module no test drives. Reaching it needs Rust to call `revise` on a store
  written in Python, which needs a session whose agent issues an `edit_memory`
  call — a scripted tool-call run, and a brittle one. The five directions either
  side of it are covered (`budget` by
  `test_a_store_written_in_python_is_asked_for_its_ceiling`, the rest by
  `test_a_python_store_is_opened_read_written_and_closed`), so what is untested
  is the forwarding, not the contract.
- **Retrieval by relevance is not expressible.** `MemoryStore::read(scope)`
  carries no query, so a store backed by an embedding index can only answer with
  the whole scope, and a retrieval store is exactly the shape the trait cannot
  hold. Adding a query is a contract change — every store gains a parameter, and
  the prompt assembler has to have something to ask about — so it is a decision
  rather than an omission, and it is not taken here.
- `SummarizingMemory` writes one filename per scope with every byte outside
  `[A-Za-z0-9_-]` expanded threefold. A scope long enough that the encoded name
  passes the filesystem's limit fails as a named `Error::Io` naming the path,
  rather than being truncated into a collision.
