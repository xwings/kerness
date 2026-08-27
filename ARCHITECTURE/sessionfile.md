# Session File

## Goal

Saving a run so it can be resumed. A snapshot holds the conversation's turns and
transcript, the orchestrator's loop state, the compaction count, and an identity
block describing the session it came from. On resume, the identity is checked
first: a snapshot from a different gameplan or a different agent roster is
refused rather than half-applied. Serves **M2**.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/sessionfile.rs` | the snapshot, the identity check, save and load |
| `crates/kerness-py/src/funcs.rs` | `PySessionSnapshot` and the four functions |
| `python/kerness/sessionfile.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/sessionfile.rs:33` — `SCHEMA_VERSION` — `1`; a file written
  by a different version is refused, not migrated.
- `crates/kerness/src/sessionfile.rs:44` — `SessionSnapshot` — identity, turns,
  transcript, loop state, compaction count.
- `crates/kerness/src/sessionfile.rs:70` — `identity_for(...)` — builds the
  identity block from the gameplan and the roster.
- `crates/kerness/src/sessionfile.rs:91` — `check_identity(saved, current)` — the
  refusal; returns `Result` and names the field that differs.
- `crates/kerness/src/sessionfile.rs:114` — `save_snapshot(path, snapshot)` — writes
  JSON.
- `crates/kerness/src/sessionfile.rs:144` — `load_snapshot(path)` — returns
  `Result<Option<_>>`: a missing file is `None`, which is how "start fresh" is
  expressed; a corrupt file is an error.

The Python constructor takes `loop=` as a keyword, which is a Rust keyword; the
binding uses the raw identifier `r#loop` so the Python name is the natural one
(`crates/kerness-py/src/funcs.rs`).

## Interactions

- Written by [session.md](session.md) after each turn when a session file is
  configured.
- Holds the turns and transcript owned by [conversation.md](conversation.md), and
  restores them together through `restore`.
- Holds the state produced by [loop.md](loop.md)'s `snapshot()`.
- Records each provider exchange through [agent-runtime.md](agent-runtime.md)'s
  `with_record` hook.

## How to Test

```sh
cargo test -p kerness sessionfile                        # pass = 0 failed
.venv/bin/python -m pytest tests/test_sessionfile.py -q  # pass = 0 failed
```

- `tests/test_sessionfile.py:31` — `test_every_kind_of_record_survives` — the round
  trip, across every record kind rather than one representative.
- `:77` — `test_a_missing_file_is_not_an_error_and_is_not_created` — why `load_snapshot`
  returns `Result<Option<_>>`.
- `:85` — `test_an_unknown_schema_version_is_refused_naming_both` — and `:139`
  `test_a_mismatch_is_refused_named_and_recoverable`: a refusal names the field, so
  the caller can act on it.

## Open Gaps / Roadmap

- Saving rewrites the whole file each time; there is no append log.
- No migration path for `SCHEMA_VERSION`. A bump invalidates every existing
  session file, which is the right default while the format is at version 1.
- The snapshot does not record tool side effects, so resume assumes an unchanged
  world — see [session.md](session.md).
