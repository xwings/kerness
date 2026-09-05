# Session File

## Goal

Saving a run so it can be resumed. A snapshot holds the conversation's turns and
transcript, the orchestrator's loop state, the compaction count, and an identity
block describing the session it came from. On resume, the identity is checked
first: a snapshot from a different gameplan or a different agent roster is
refused rather than half-applied. M2 adds durable continuations at provider,
tool, approval, and scheduler boundaries.

## Status

`done` — M2 writes version 2 continuations, reads valid version 1 boundaries,
validates the shared envelope, and syncs action-intent snapshots before tool
execution.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/sessionfile.rs` | the snapshot, the identity check, save and load |
| `bindings/python/src/funcs.rs` | `PySessionSnapshot` and the four functions |
| `bindings/python/kerness/sessionfile.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/sessionfile.rs:36` — `SCHEMA_VERSION` — writes version 2;
  valid version 1 turn-boundary files remain readable.
- `crates/kerness/src/sessionfile.rs:47` — `SessionSnapshot` — identity, turns,
  transcript, loop state, compaction count.
- `crates/kerness/src/sessionfile.rs:74` — `identity_for(...)` — builds the
  identity block from the gameplan and the roster.
- `crates/kerness/src/sessionfile.rs:95` — `check_identity(saved, current)` — the
  refusal; returns `Result` and names the field that differs.
- `crates/kerness/src/sessionfile.rs:121` — `save_snapshot(path, snapshot)` — writes
  JSON.
- `crates/kerness/src/sessionfile.rs:183` — `load_snapshot(path)` — returns
  `Result<Option<_>>`: a missing file is `None`, which is how "start fresh" is
  expressed; a corrupt file is an error.

The Python constructor takes `loop=` as a keyword, which is a Rust keyword; the
binding uses the raw identifier `r#loop` so the Python name is the natural one
(`bindings/python/src/funcs.rs`).

Version 2 stores the suspended runtime inside the existing `loop.runtime`
object, preserving the public `SessionSnapshot` struct layout and Python
constructor. The engine owns that continuation's schema: pending provider/tool
actions, approval identity, completed results, scheduler progress, correlation
IDs, and usage accounting. Providers and handlers are re-registered, not
serialized. A version 1 file cannot contain a runtime or scheduler continuation;
the engine migrates its valid turn boundary without inventing suspended work.

Both readable versions require the complete outer envelope, four typed identity
fields, structured turn/transcript entries, and nonnegative integer counters.
Unknown envelope/record/legacy-loop fields and malformed values are errors,
rather than a source of silent empty strings or reset counters. Standalone
snapshots may have an empty loop; optional legacy phase fields are type-checked
when present. Runtime and scheduler owners validate their continuation objects
and registered identities when a run resumes.

Turn and transcript records use the conversation types' serde implementations
after envelope validation, keeping one field definition for each record type.

Saving exclusively creates a sibling temporary file with `create_new`, using
a fresh process/counter suffix when the first name is occupied. Existing
files and symlinks are left untouched. Bytes go through the opened handle;
only a successful write and file sync are renamed over the destination. Write
or rename failure removes the file this save created and leaves the previous snapshot
intact. On Unix, temporary files are created with owner-only permissions, and
the parent directory is synced after rename. A directory-sync failure is
reported even though the new snapshot has already replaced the old one;
callers must not infer that a failed save always left the old file in place.

## Interactions

- Written by [run.md](run.md) at explicit runtime boundaries when a
  session file is configured, including action intent before side effects and
  completion afterward.
- Holds the turns and transcript owned by [conversation.md](conversation.md), and
  restores them together through `restore`.
- Holds the state produced by [loop.md](loop.md)'s `snapshot()`.
- Holds [provider.md](provider.md)'s usage ledger in the version 2 continuation.
- Records each provider exchange through [agent-runtime.md](agent-runtime.md)'s
  `with_record` hook.

## How to Test

```sh
cargo test -p kerness --lib sessionfile
cargo test -p kerness --test resume
.venv/bin/python -m pytest bindings/python/tests/test_sessionfile.py -q
```

Pass means exit code 0 and no failed tests. Rebuild the Python extension before
running its tests after a Rust change.

The core-upgrade verification passed all eight session-file Rust tests, all 12
resume integration tests, and the rebuilt Python suite. The complete
workspace gate is recorded in [testing.md](testing.md).

- `crates/kerness/src/sessionfile.rs:367` — Typed record roundtrip, version 1 boundary reading, and version 2 continuation preservation.
- `crates/kerness/src/sessionfile.rs:484` — Missing, mistyped, negative, unknown, and version-incompatible shared fields reject.
- `crates/kerness/src/sessionfile.rs:545` — Exclusive temporary creation, private permissions, symlink collision safety, replacement, and failure cleanup.
- `crates/kerness/tests/resume.rs:422` — Repeated restore/checkpoint preserves prior committed turns, phase state, approval identity and native call progress; malformed counters and forged identities reject.
- `crates/kerness/tests/resume.rs:625` — Captured tool intents require reconciliation or cancellation without replay, including completion-save failure.
- `crates/kerness/tests/resume.rs:771` — An intent-save failure prevents execution.

## Open Gaps / Roadmap

- Saving rewrites the whole file each time; there is no append log.
- An intent without a completion is indeterminate after a crash. The run
  requires explicit host reconciliation instead of replaying an arbitrary
  external side effect; there is no exactly-once guarantee.
- File replacement is atomic on supported local filesystems. Durability still
  depends on filesystem and storage sync guarantees; directory sync is Unix
  only.
