# Memory

## Goal

A Markdown file agents read at the start of a turn and append to during it. It
is the only state that outlives a turn without being in the conversation, and it
is deliberately free-form prose rather than a key-value store: what an agent
writes there goes into the next agent's prompt verbatim.

Two consequences follow from *verbatim*, and both are the module's business
rather than its callers':

- The file is a channel between agents. Text arriving from a model is filtered
  on the way in and quoted on the way out.
- The file outlives the run that wrote it. A session resumed a week later reads
  week-old notes, so the file's age travels with its content into the prompt.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the file, its age, its operations, and the filter trait |
| `crates/kerness/src/session.rs:244` | `remember`, the one path model output takes into the file |
| `bindings/python/src/session.rs:175` | `PyFilter`, a Python callable behind the trait |
| `bindings/python/src/types.rs:998` | `PyMemory`, and the owned-vs-session distinction |
| `bindings/python/kerness/memory.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:53` — `Memory` — a path and the loaded content.
- `crates/kerness/src/memory.rs:100` — `load()` — reads the file; a missing file is
  empty content, not an error, because the first session creates it.
- `crates/kerness/src/memory.rs:111` — `read()` — the content, borrowed; this is
  what goes into the prompt.
- `crates/kerness/src/memory.rs:116` — `append(text)` / `:127` `append_entry(text)` —
  raw append versus an append that adds the entry separator; the second is what
  every write in a session goes through.
- `crates/kerness/src/memory.rs:142` — `write(content)` — replaces the file.
- `crates/kerness/src/memory.rs:86` — `age()` — whole days since the file was last
  written, or `None` when there is no file to date.
- `crates/kerness/src/memory.rs:43` — `MemoryFilter` — one method,
  `filter(note, actor)`, returning the text to store or `None` to drop it.
- `bindings/python/src/types.rs:1004` — `of_session(memories)` on `PyMemory` — a
  memory handle backed by the session's live store rather than an owned copy.

### Age, read from the filesystem

`age()` reads the mtime rather than parsing the content, because the module
imposes no format on the content and a timestamp parsed out of prose would be
one. A clock that has gone backwards since the write reads as `0` rather than as
an error: staleness is advisory, and no caveat is the better answer when the age
is not credible.

`None` and `Some(0)` are distinct and both reach the prompt.
[prompting.md](prompting.md)'s `memory_block` takes the age and renders a caveat
past `MEMORY_STALE_AFTER_DAYS`; a file that does not exist yet holds only notes
written this run, which are as fresh as the run, so it carries none.

### The trust boundary

What an agent writes lands inside another agent's *system prompt*, which is the
position a session's own instructions occupy. Two separate mechanisms answer
that, at the two ends of the file:

- On the way in, `MemoryFilter` (`memory.rs:43`). It is applied in `remember`
  (`session.rs:244`), a free function both write paths call — the `write_memory`
  tool (`session.rs:1940`) and the `@MEMORY:` marker pass (`session.rs:1427`) —
  so a caller who installs a filter cannot have it cover one and miss the other.
  A dropped note is reported to the writer as *not saved*, without saying which
  rule refused it: a specific rejection teaches a model how to word the next
  attempt.
- On the way out, `MEMORY_CAVEAT` and the `MEMORY_BEGIN`/`MEMORY_END` fence in
  [prompting.md](prompting.md). The block says plainly that it is recorded
  material, not instruction.

The framework ships no filter implementation. What counts as a secret, and what
a session is willing to persist, are the caller's to define; a redactor guessing
at it here would be wrong in both directions and wrong silently.

Two things are deliberately outside the boundary. A caller writing through
`Memory` directly is not filtered, because the caller is not the untrusted
party — and neither is the session's own closing `## Session Result` block
(`session.rs:936`), which the framework composes rather than a model. From
Python, a filter that raises drops the note and logs a warning
(`bindings/python/src/session.rs:186`): the trait has no error path, and the safe
reading of a filter that could not decide is that the note stays out — but
silence would make that indistinguishable from a deliberate `None`.

### Owned versus session-backed

`PyMemory` holds a `Store` enum: either an owned `Memory` or a handle on the
session's `Arc<Mutex<Memories>>`. The distinction is observable —
`session.memory.read()` after `run()` has to show what the run wrote, which an
owned snapshot taken at construction would not. The same reasoning drives
`PromptAssembler`'s `memory_for` callback: it hands back the agent's `Memory`,
not its text, so two agents pointed at one file both see each other's writes.
`memory_age_for` is a second callback for the same reason — the age is read per
turn, so a file written mid-run stops being stale without the session rebuilding
anything.

## Interactions

- Rendered into a system prompt by [prompting.md](prompting.md)'s `memory_block`,
  which also owns the caveat and the staleness line.
- Owned per session by [session.md](session.md)'s `Memories` (`session.rs:214`),
  which also holds the configured `memory_filter`.
- Written through the `write_memory` tool registered by [toolkit.md](toolkit.md).
- Memory markers in a reply are extracted by [utils.md](utils.md)'s
  `parse_memory_markers`.
- Counted as prompt overhead by [compaction.md](compaction.md): the file is part
  of the system message, so it narrows what the conversation may use.

## How to Test

```sh
cargo test -p kerness memory                                        # pass = 0 failed
cargo test -p kerness --lib session                                 # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_memory.py -q  # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q # pass = 0 failed
```

- `crates/kerness/src/memory.rs:219` —
  `a_file_written_now_is_zero_days_old_and_an_absent_one_has_no_age` — the two
  cases the prompt distinguishes.
- `crates/kerness/src/session.rs:2921` —
  `a_filter_rewrites_or_drops_what_an_agent_writes` — the filter at the layer
  that applies it, not through a whole session.
- `bindings/python/tests/test_memory.py:7` — `test_load_reads_what_is_there_and_creates_what_is_not`;
  `:43` `test_an_entry_is_stored_verbatim_one_blank_line_apart`; `:63`
  `test_nothing_reaches_disk_until_there_is_something_to_write`; and `:77`
  `test_age_is_none_without_a_file_and_whole_days_once_there_is_one`, which
  backdates an mtime rather than waiting a day, and is where `Option<u64>`
  arriving as `None` or an `int` is proven.
- `bindings/python/tests/test_session.py:1999` —
  `test_a_filter_sees_every_note_and_can_rewrite_or_drop_it`; `:2035`
  `test_a_filter_returning_none_keeps_the_note_out_of_the_file`; `:2063`
  `test_a_filter_that_raises_drops_the_note_and_says_so`, the gate failing
  closed; `:2101` `test_a_filter_that_is_not_callable_is_refused_at_construction`,
  which is refused at construction rather than at the first note an agent writes.
- `bindings/python/tests/test_session.py:2114` — `test_per_agent_memory` — and `:2163`
  `test_agent_without_memory_uses_session_memory`: the owned-versus-session
  distinction, observed through a live session.

## Open Gaps / Roadmap

- Every append rewrites through the loaded content; a memory file that grows very
  large is re-read and re-written each time.
- No locking beyond the session's own mutex. Two processes pointed at one memory
  file will interleave writes.
- No size ceiling. A file large enough to fill the context window is a named
  error from `fit_conversation` (`session.rs:1593`) rather than a silent
  degradation, but the framework will not trim the file to avoid it: which notes
  are worth keeping is the caller's judgement, not the framework's.
