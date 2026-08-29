# Memory

## Goal

A Markdown file agents read at the start of a turn and append to during it. It
is the only state that outlives a turn without being in the conversation, and it
is deliberately free-form prose rather than a key-value store: what an agent
writes there goes into the next agent's prompt verbatim.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/memory.rs` | the file and its four operations |
| `bindings/python/src/types.rs:981` | `PyMemory`, and the owned-vs-session distinction |
| `bindings/python/kerness/memory.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/memory.rs:19` — `Memory` — a path and the loaded content.
- `crates/kerness/src/memory.rs:48` — `load()` — reads the file; a missing file is
  empty content, not an error, because the first session creates it.
- `crates/kerness/src/memory.rs:59` — `read()` — the content, borrowed; this is
  what goes into the prompt.
- `crates/kerness/src/memory.rs:64` — `append(text)` / `:75` `append_entry(text)` —
  raw append versus an append that adds the entry separator; the second is what
  the `write_memory` tool calls.
- `crates/kerness/src/memory.rs:90` — `write(content)` — replaces the file.
- `bindings/python/src/types.rs:987` — `of_session(memories)` on `PyMemory` — a
  memory handle backed by the session's live store rather than an owned copy.

### Owned versus session-backed

`PyMemory` holds a `Store` enum: either an owned `Memory` or a handle on the
session's `Arc<Mutex<Memories>>`. The distinction is observable —
`session.memory.read()` after `run()` has to show what the run wrote, which an
owned snapshot taken at construction would not. The same reasoning drives
`PromptAssembler`'s `memory_for` callback: it hands back the agent's `Memory`,
not its text, so two agents pointed at one file both see each other's writes.

## Interactions

- Rendered into a system prompt by [prompting.md](prompting.md)'s `memory_block`.
- Owned per session by [session.md](session.md)'s `Memories` (`session.rs:193`).
- Written through the `write_memory` tool registered by [toolkit.md](toolkit.md).
- Memory markers in a reply are extracted by [utils.md](utils.md)'s
  `parse_memory_markers`.

## How to Test

```sh
cargo test -p kerness memory                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_memory.py -q # pass = 0 failed
```

- `bindings/python/tests/test_memory.py:7` — `test_load_reads_what_is_there_and_creates_what_is_not`;
  `:43` `test_an_entry_is_stored_verbatim_one_blank_line_apart`; `:63`
  `test_nothing_reaches_disk_until_there_is_something_to_write`.
- `bindings/python/tests/test_session.py:1795` — `test_per_agent_memory` — and `:1844`
  `test_agent_without_memory_uses_session_memory`: the owned-versus-session
  distinction, observed through a live session.

## Open Gaps / Roadmap

- Every append rewrites through the loaded content; a memory file that grows very
  large is re-read and re-written each time.
- No locking beyond the session's own mutex. Two processes pointed at one memory
  file will interleave writes.
- There is no size ceiling, so a memory file large enough to dominate the context
  window is possible; [compaction.md](compaction.md) does not count it.
