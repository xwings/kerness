# Memory

## Goal

What the agents in a session remember, and where it is kept. It is the only
state that outlives a turn without being in the conversation, and what an agent
writes there goes into the next agent's prompt verbatim.

Memory is a slot. A session holds one `MemoryStore` and addresses it by *scope*
— a name whose meaning is the store's to decide. The default store, `FileMemory`,
reads a scope as a path and keeps free-form Markdown prose there rather than a
key-value structure. Two bundled stores bound what a scope may grow to, and they
differ in *who* does the bounding:

- `SummarizingMemory` keeps the most recent entries word for word and folds the
  rest into a running summary through one provider call at the end of the run.
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

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the store trait, the three bundled stores, the file primitive, and the filter trait |
| `crates/kerness/src/session.rs:226` | `Memories` — the store plus the scope each agent addresses it by |
| `crates/kerness/src/session.rs:267` | `remember`, the one path model output takes into memory |
| `crates/kerness/src/session.rs:305` | `revise_memory`, the same path for a revision |
| `bindings/python/src/memory.rs` | the boundary: a Python store seen as one, and the bundled store seen from Python |
| `bindings/python/src/session.rs:176` | `PyFilter`, a Python callable behind the filter trait |
| `bindings/python/src/types.rs:972` | `PyMemory`, the file primitive on its own |
| `bindings/python/kerness/memory.py` | the `MemoryStore` ABC and the re-exports |

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:151` — `MemoryStore` — `read(scope)` and
  `append(scope, note)` are required; `open`, `age`, `path`, `close`, `budget`,
  and `revise` are defaulted, because they are the six a store can honestly have
  no answer for.
- `crates/kerness/src/memory.rs:171` — `open(scope)` — called once per scope at
  the top of `run()`. The point is *when*: a store that cannot reach what it is
  backed by says so before the first provider call rather than mid-turn.
- `crates/kerness/src/memory.rs:205` — `close()` — called once at the end of
  `run()`, after the session result. The only moment at which the whole run is
  known and nothing further will be appended, so it is where a store that
  consolidates does it.
- `crates/kerness/src/memory.rs:224` — `budget()` — the character ceiling one
  scope may hold, or `None`. Answering is what makes a store *curated*; see
  [below](#curatedmemory).
- `crates/kerness/src/memory.rs:245` — `revise(scope, old, new)` — replace the one
  entry containing `old`, or remove it when `new` is empty. The default refuses
  rather than silently doing nothing.
- `crates/kerness/src/memory.rs:267` — `FileMemory` — the default: one `Memory`
  per scope behind a `Mutex`, opened on demand. `Mutex` and not `RwLock` because
  reading an unopened scope loads it, which is a write to the map.
- `crates/kerness/src/memory.rs:327` — `Memory` — a path and the loaded content,
  the file primitive `FileMemory` is built out of and usable on its own.
  `load()` at `:363` treats a missing file as empty content rather than an error;
  `append_entry(text)` at `:390` adds the entry separator and is what every write
  in a session goes through; `age()` at `:355` is whole days since the file was
  last written.
- `crates/kerness/src/memory.rs:454` — `SummarizingMemory` — the second bundled
  store; see [below](#summarizingmemory).
- `crates/kerness/src/memory.rs:710` — `CuratedMemory` — the third; see
  [below](#curatedmemory).
- `crates/kerness/src/memory.rs:70` — `days_since_write(path)` — whole days since
  a file was last written, shared by all three bundled stores.
- `crates/kerness/src/memory.rs:89` — `scope_file(root, scope, extension)` — the
  `%XX` encoding that turns a scope into one filename under a root, shared by the
  two stores that keep a root.
- `crates/kerness/src/memory.rs:105` — `write_creating_parent(path, content)` — the
  other half the stores share: the directory is made by the first real write,
  not when a scope is opened.
- `crates/kerness/src/memory.rs:126` — `MemoryFilter` — one method,
  `filter(note, actor)`, returning the text to store or `None` to drop it.
- `crates/kerness/src/session.rs:234` — `scope_for(agent)` — the agent's own
  scope when it declared one, the session's otherwise. This is what makes memory
  shared by default and private only on request.
- `bindings/python/src/memory.rs:136` — `bind_memory_store(object)` — a Python
  store seen as a `MemoryStore`, with an exact-type shortcut past each bundled
  store.
- `bindings/python/src/memory.rs:331` — `PySessionMemory` — what
  `session.memory` returns: the live store at the session scope, not a snapshot.

### Scope, and why it is a string

`SessionConfig::memory` and `Agent::memory` are strings the session never
parses. `FileMemory` reads one as a path; another store reads it as a key, a
collection name, or a namespace. Nothing above `MemoryStore` assumes memory is a
file, which is what lets a store be replaced without the session, the prompt
assembler, or the two memory tools changing.

The session takes the store and the scope together, under one lock, and then
calls the store with the lock released — `store_for` (`session.rs:328`). A store
written in Python can run arbitrary code, including code that re-enters the
session; holding the session's lock across that call is how it would deadlock.

### SummarizingMemory

The second bundled store, and the reason the slot is worth having: notes that
only ever grow eventually cost more of every prompt than they are worth. It
keeps one JSON file per scope under a root, holding a running summary and the
entries written since that summary was last rewritten. `read` renders the
summary — labelled `CONSOLIDATED_PREFIX` (`memory.rs:427`), so an agent can tell
a framework-written recap from a note somebody wrote — and then the entries.
`append` writes through on every note, so a crash mid-run loses nothing that was
committed.

The consolidation happens in `close()` (`memory.rs:637`), once, at the end of
the run: one provider call per scope whose entries have outgrown
`with_keep(entries)` (`memory.rs:542`), carrying the running summary and the
overflow and getting back a rewritten summary. The end of the run is the only
honest moment for it — the first point at which the whole run is known and the
last at which nothing further will be appended — and doing it mid-turn would
charge an agent's own turn for rewriting notes it is about to read.

Two decisions are worth naming:

- **The provider is required at construction** (`memory.rs:522`). A store built
  without one would keep every entry forever, which is what `FileMemory` already
  does, and it would do it silently.
- **A failed consolidation is not a failed run** (`memory.rs:584`). The call
  returns `None` and the scope is left exactly as its agents wrote it. This is
  [compaction.md](compaction.md)'s rule inverted: there, a failed summary means
  keeping turns that would have been dropped; here it means keeping notes that
  would have been rewritten. Both preserve what was actually written, and losing
  a run's notes to a network error is the worse of the two outcomes by a
  distance.

### CuratedMemory

The third bundled store, and the other answer to the same problem: a scope is
held to `budget()` characters — `DEFAULT_MEMORY_BUDGET` (`memory.rs:676`) is
2,200, roughly 550 tokens at [compaction.md](compaction.md)'s `CHARS_PER_TOKEN`
— and the agents are the ones who keep it under. One
Markdown file per scope under a root, entries joined by `ENTRY_SEPARATOR`
(`memory.rs:685`) on lines of their own, so a scope stays a file somebody can
read and hand-edit.

Four decisions carry the design:

- **It does not compact.** An append that would cross the ceiling is an
  `Error::Value` (`memory.rs:798`) carrying the figure it would have reached and
  the entries as they stand, telling the writer to merge or remove and write
  again. The agent is mid-turn and has the tool to do it, and the alternative —
  dropping the oldest note to make room — discards the caller's material on a
  guess about which note mattered least.
- **An entry is addressed by a fragment of itself.** `revise` takes any substring
  appearing in exactly one entry; `locate` (`memory.rs:815`) refuses a fragment
  matching none or several and names which, because rewriting a guess is the one
  failure the writer cannot detect. The replacement replaces the whole entry, not
  the fragment, so a revision is never a blind splice.
- **`read` leads with the usage line** (`memory.rs:846`) — characters used, the
  ceiling, and the entry count — because an agent that cannot see how full the
  scope is cannot be asked to make room in it. An empty scope reads as the empty
  string, so `memory_block` renders nothing at all rather than `0 of 2,200`.
- **An exact duplicate is accepted and not stored twice** (`memory.rs:861`). A
  model re-writing a note it already wrote has made no mistake worth an error,
  and spending the ceiling on a second copy is the outcome nobody wants.

Answering `budget()` is also what makes the session offer the `edit_memory` tool
(`session.rs:2105`). The gate is deliberate: a store that keeps notes append-only
takes the trait's `revise` default, which refuses, and advertising a tool whose
every call would be refused is worse than not offering it.

### A scope is a key, not a path

Both stores that keep a root put every scope through `scope_file`
(`memory.rs:89`), which writes every byte outside `[A-Za-z0-9_-]` as `%XX`. The
encoding is reversible — so two scopes never collide on one file — and leaves no
separator and no `.` in the name, so a scope reading like `../../elsewhere` names
a file *under* the root rather than one outside it. Both stores still answer
`path(scope)`, so whatever they name is confined by the workspace as well; the
encoding is what makes them correct on their own rather than only correct because
something above them checked.

### Age, read from the filesystem

All three bundled stores read the mtime through `days_since_write`
(`memory.rs:70`) rather than parsing the content, because none imposes a format
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

- On the way in, `MemoryFilter` (`memory.rs:126`). It is applied in `remember`
  (`session.rs:267`), a free function both write paths call — the `write_memory`
  tool (`session.rs:2068`) and the `@MEMORY:` marker pass (`session.rs:1553`) —
  so a caller who installs a filter cannot have it cover one and miss the other.
  A dropped note is reported to the writer as *not saved*, without saying which
  rule refused it: a specific rejection teaches a model how to word the next
  attempt.
- On the way out, `MEMORY_CAVEAT` and the `MEMORY_BEGIN`/`MEMORY_END` fence in
  [prompting.md](prompting.md). The block says plainly that it is recorded
  material, not instruction.

A revision is model output landing in the same place, so it takes the same route:
`revise_memory` (`session.rs:305`) is `remember`'s counterpart and the only path
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
`## Session Result` block (`session.rs:1046`), which the framework composes
rather than a model. From Python, a filter that raises drops the note and logs a
warning (`bindings/python/src/session.rs:187`): the trait has no error path, and
the safe reading of a filter that could not decide is that the note stays out —
but silence would make that indistinguishable from a deliberate `None`.

### Failing early, and failing soft

The two directions are deliberate and different.

`open()` is fallible and runs before the first turn, so an unreachable store
costs nothing (`session.rs:1006`). `append()`, `revise()`, and `close()` are
fallible and propagate, because a note that was not stored is a result the caller
must see — and for `revise` the refusal *is* the message the agent acts on.

`memory_text` (`session.rs:415`) is the exception: it logs and yields an empty
block rather than failing the run. `PromptAssembler` takes an infallible
`Fn(&Agent) -> String`, and a read that failed *this late* would discard provider
calls already paid for — every scope was opened successfully before the first
turn, so a failure here arose mid-run.

The three infallible trait methods take the same shape at the binding.
`PyStore`'s `optional` (`bindings/python/src/memory.rs:47`) logs and answers
`None` when a Python `age`, `path`, or `budget` raises: those return `Option`,
not `Result`, and the honest reading of a store that cannot name its file is a
store that names none — which is what a store keeping nothing on disk answers
anyway. A store that cannot name a ceiling is read the same way, and simply is
not offered `edit_memory`.

### The Python surface

`MemoryStore` is an ABC in `bindings/python/kerness/memory.py:31` for the reason
`Channel` is one ([bindings.md](bindings.md)): it is what callers subclass, and
an extension cannot declare an abstract base class. All three bundled stores are
registered against it at `:118`–`:120` rather than inheriting from it, so
`isinstance` holds without an extension type subclassing a Python ABC.

`budget` and `revise` are concrete on the ABC (`memory.py:86` and `:97`), and
`revise` raises `ValueError` rather than returning. The exception class is the
choice that matters: `ValueError` crosses back as `Error::Value`, which is what
the Rust default returns, so a Python store that does not override it and a Rust
store that does not are indistinguishable to the session. The message is
`REVISE_UNSUPPORTED` (`memory.rs:257`), imported rather than respelled: the two
halves of one default spelled out in two languages drift silently, and this one
is text a caller reads.

`SummarizingMemory(root, provider, model, keep=DEFAULT_KEEP_ENTRIES)`
(`bindings/python/src/memory.rs:218`) binds its provider exactly the way an
agent's is bound, so a `Provider` subclass written in Python is what does the
summarising when one is passed. `CuratedMemory(root, budget=DEFAULT_MEMORY_BUDGET)`
(`bindings/python/src/memory.rs:280`) needs no provider and forwards all eight
trait methods.

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
  against the workspace at `Session::new` (`session.rs:594`) and, for a per-agent
  scope, at the top of `run()` (`session.rs:987`).
- Read and written through the `read_memory`, `write_memory`, and `edit_memory`
  tools registered by [toolkit.md](toolkit.md); the last is offered only when the
  store answers `budget()`.
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

- `crates/kerness/src/memory.rs:983` —
  `a_file_written_now_is_zero_days_old_and_an_absent_one_has_no_age` — the two
  cases the prompt distinguishes.
- `crates/kerness/src/memory.rs:1007` — `the_default_store_keeps_one_file_per_scope`
  and `:1026` `a_scope_never_opened_still_reads_what_is_on_disk`, which is the
  lazy load in `FileMemory::with` proving a caller need not call `open`.
- `crates/kerness/src/memory.rs:1067` —
  `a_store_writing_no_file_answers_the_defaults_and_leaves_no_trace` — the
  smallest possible store, and the defaults answering for it.
- `crates/kerness/src/memory.rs:1152` —
  `entries_read_back_verbatim_until_something_consolidates_them` and `:1169`
  `closing_folds_everything_past_the_kept_entries_into_one_summary`: one call, at
  the end, carrying only what overflowed. `:1191`
  `a_second_consolidation_is_given_the_first_one_to_build_on` is why the summary
  leads that request.
- `crates/kerness/src/memory.rs:1211` —
  `a_failed_consolidation_keeps_the_notes_as_they_were_written` — a provider
  failure is not a session failure, and nothing is lost to it. `:1226`
  `a_scope_survives_the_store_that_wrote_it` reads a consolidated scope back
  through a second store.
- `crates/kerness/src/memory.rs:1243` —
  `a_scope_is_a_key_and_never_a_path_out_of_the_root` — the encoded filename, and
  that two scopes differing only in a separator do not share one file. `:1482`
  `a_curated_scope_is_a_key_and_never_a_path_out_of_the_root` is the same
  guarantee for the store that shares `scope_file`.
- `crates/kerness/src/memory.rs:1278` —
  `a_store_with_no_ceiling_refuses_to_revise_and_says_so` — the trait default
  refusing rather than pretending.
- `crates/kerness/src/memory.rs:1293` —
  `entries_read_back_behind_a_line_saying_how_full_the_scope_is` — the usage line
  the agent budgets against; `:1318`
  `a_note_already_stored_word_for_word_is_accepted_and_not_stored_twice`.
- `crates/kerness/src/memory.rs:1331` —
  `an_append_past_the_ceiling_is_refused_and_says_what_is_stored` — the refusal,
  and that nothing was mutated on the way to it. `:1425`
  `a_revision_past_the_ceiling_is_refused_and_the_entry_survives` is the same
  guarantee on the revise path.
- `crates/kerness/src/memory.rs:1354` —
  `revising_replaces_the_whole_entry_a_fragment_addresses`; `:1375`
  `revising_to_nothing_removes_the_entry`; `:1390`
  `a_fragment_matching_none_or_several_changes_nothing_and_names_which`, which is
  the ambiguity guard.
- `crates/kerness/src/memory.rs:1442` —
  `a_curated_scope_survives_the_store_that_wrote_it` and `:1460`
  `a_hand_edited_file_loads_as_the_entries_it_visibly_holds` — the file is a
  format somebody can edit, so a file edited by hand has to load as what it
  looks like.
- `crates/kerness/src/session.rs:3339` —
  `an_installed_store_is_opened_read_written_and_closed` — the slot end to end,
  including that every `open` precedes every `read` and that a per-agent scope
  routes to itself.
- `crates/kerness/src/session.rs:3406` —
  `the_filter_runs_before_an_installed_store_sees_a_note` — the ordering the
  trust boundary rests on: two calls to the tool, one append.
- `crates/kerness/src/session.rs:3471` —
  `a_store_that_names_no_file_is_checked_against_no_workspace` — the default's
  path refused, a store answering `None` left alone. `:3500`
  `a_store_that_cannot_open_stops_the_run_before_the_first_turn` is the other
  half: no provider was called.
- `crates/kerness/src/session.rs:3107` —
  `a_filter_rewrites_or_drops_what_an_agent_writes` — the filter at the layer
  that applies it, not through a whole session.
- `crates/kerness/src/session.rs:3164` —
  `edit_memory_is_offered_only_where_the_store_sets_a_ceiling` — the gate, and
  that the description names the figure the agent is held to. `:3213`
  `edit_memory_revises_through_the_filter_and_removes_without_it` is the trust
  boundary on the revise path: a filtered replacement never reaches the store.
- `bindings/python/tests/test_memory.py:23` — `test_load_reads_what_is_there_and_creates_what_is_not`;
  `:59` `test_an_entry_is_stored_verbatim_one_blank_line_apart`; `:79`
  `test_nothing_reaches_disk_until_there_is_something_to_write`; and `:93`
  `test_age_is_none_without_a_file_and_whole_days_once_there_is_one`, which
  backdates an mtime rather than waiting a day, and is where `Option<u64>`
  arriving as `None` or an `int` is proven.
- `bindings/python/tests/test_memory.py:119` —
  `test_the_base_class_answers_for_a_store_that_keeps_no_file`, including that
  `budget` answers `None` and `revise` raises; `:146`
  `test_the_bundled_stores_are_memory_stores`, the virtual subclassing; and
  `:174` `test_a_store_that_raises_reaches_the_caller_as_what_it_raised`, which
  is where the fallible round trip through `Catch`/`Raise` is proven.
- `bindings/python/tests/test_memory.py:199` — `TestSummarizingMemory` — the
  second store across the boundary: nothing rewritten before `close`, one
  provider call when it comes, the encoded filename, and `:243`
  `test_a_session_can_be_told_to_keep_its_memory_in_one`, which is the slot
  itself — a session addressing a store that keeps no prose and never learning
  it does not.
- `bindings/python/tests/test_memory.py:268` —
  `test_the_ceiling_is_the_crate_default_or_the_keyword_that_overrides_it` — the
  constructor's own keyword, and `DEFAULT_MEMORY_BUDGET` arriving as the figure
  the crate holds rather than one Python repeats. What the ceiling *does* is
  asserted in the crate and not restated here. `:277`
  `test_a_session_can_be_told_to_keep_its_memory_in_one` is the slot with the
  store that has one, and `:293`
  `test_a_store_written_in_python_is_asked_for_its_ceiling` is the direction only
  a live run exercises — Rust asking a Python store for its `budget`.
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
  window is a named error from `fit_conversation` (`session.rs:1711`) rather than
  a silent degradation, but `FileMemory` will not trim the file to avoid it:
  which notes are worth keeping is the caller's judgement. `SummarizingMemory`
  and `CuratedMemory` are where that judgement is made — each bounds its own
  `read()` by construction — and a caller who wants a bound takes one.
- The three bundled stores are three pyclasses each forwarding eight one-line
  trait methods (`bindings/python/src/memory.rs:164`, `:218`, `:280`). A macro
  was considered and declined: pyo3 0.23 needs the `multiple-pymethods` feature
  to split a `#[pymethods]` block, and `#[pymethods]` will not expand a
  `macro_rules!` invocation inside the impl, so the only shape that works wraps
  the whole block — more machinery than the sixteen lines it saves. A fourth
  bundled store is the point at which that trade changes.
- `PyStore::revise` (`bindings/python/src/memory.rs:105`) is the one crossing in
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
