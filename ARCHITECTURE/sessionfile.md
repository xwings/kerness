---
eatmycode_version: "1.1.0"
---

# Session File

## Goal

Saving a run so it can be resumed. A snapshot holds the conversation's turns and
transcript, the orchestrator's loop state, the compaction count, and an identity
block describing the session it came from. On resume, the identity is checked
first: a snapshot from a different gameplan or a different agent roster is
refused rather than half-applied. This is M2's durable continuation boundary at
provider, tool, approval and scheduler steps.

This module owns the file's envelope, the identity check, and the atomic
write. It does not own what goes inside `loop`: the scheduler object belongs to
[loop.md](loop.md) and the `runtime` continuation to [run.md](run.md), and each
validates its own object when a run resumes. It is not memory: memory is
durable user-owned prose with no schema ([memory.md](memory.md)); a session
file is machine state for exactly one run.

## Status

`done` — the writer emits schema version 2, the reader accepts valid version 1
turn-boundary files and version 2 continuations, the shared envelope is
validated on both paths, and action intent is synced before a tool executes.
`cargo test -p kerness --lib sessionfile` passes 8 tests, `cargo test -p
kerness --test resume` passes 12, and
`bindings/python/tests/test_sessionfile.py` passes 11.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/sessionfile.rs` | `SCHEMA_VERSION`, `SessionSnapshot`, the identity functions, atomic `save_snapshot`, validating `load_snapshot` |
| `crates/kerness/src/session/run.rs` | `checkpoint`, the caller that fills `loop.runtime`; documented in [run.md](run.md) |
| `crates/kerness/src/session.rs` | `resume` and `save`, the compatibility adapter's reader and writer |
| `bindings/python/src/funcs.rs:550` | `PySessionSnapshot` and the four functions |
| `bindings/python/kerness/sessionfile.py` | Re-export shim: `SCHEMA_VERSION`, `SessionSnapshot`, `check_identity`, `identity_for`, `load_snapshot`, `save_snapshot` |

## Language and Conventions

One Rust crate module, a block of one PyO3 binding module, one Python shim. The
root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `sessionfile.rs` imports `conversation`, `error` and `pyfmt` only; the
  turn and transcript records reuse the conversation types' serde
  implementations after envelope validation, so each record has one field
  definition. `SessionSnapshot` itself derives no serde: the envelope is built
  by hand at `crates/kerness/src/sessionfile.rs:129` and read by hand at
  `:183`, which is what lets the reader reject rather than default.
- The file is written through `pyfmt::json_dumps_indent2` (`:165`) so a
  snapshot compares byte-for-byte with what Python's `json.dumps(indent=2)`
  produces; [utils.md](utils.md) owns that guarantee.
- Two `#[cfg(unix)]` sites: owner-only `0o600` on the temporary file (`:135`)
  and `sync_parent` (`:233`), whose non-Unix twin at `:242` is a no-op.
- Every IO failure is `Error::Io` naming the path; every malformed-content
  failure is `Error::Session` naming the file and the two ways out ("Delete it
  to start fresh, or pass a different session_file path"). Observed convention,
  asserted by the tests below.
- The Python constructor takes `loop=` as a keyword, which is a Rust keyword;
  the binding uses the raw identifier `r#loop`
  (`bindings/python/src/funcs.rs:582`) and names the getter with
  `#[pyo3(name = "loop")]` so the Python name is the natural one.
- Unit tests are inline (`crates/kerness/src/sessionfile.rs:350`) on
  `crate::testing::TempDir` (`:355`);
  integration tests in `crates/kerness/tests/resume.rs` use the doubles in
  `crates/kerness/tests/common/mod.rs`; the Python tests build snapshots from
  `Conversation`, `Turn` and the `IDENTITY` constant at
  `bindings/python/tests/test_sessionfile.py:24`.

## Design and Invariants

### One JSON object, rewritten whole

The file is a single object rewritten on every save. Appending would be cheaper
per turn, but compaction rewrites history rather than extending it, so an
append-only log would need a rewrite path anyway; and the `.jsonl` shape is
already taken by [channel.md](channel.md)'s `LogChannel`, which is transcript
output rather than resumable state.

### Two readable versions, one envelope

`SCHEMA_VERSION` (`crates/kerness/src/sessionfile.rs:36`) is 2. Version 2
stores the suspended runtime inside the existing `loop.runtime` object and the
scheduler under `loop.scheduler`, preserving the public `SessionSnapshot`
struct layout and the Python constructor. The engine owns that continuation's
schema: pending provider and tool actions, approval identity, completed
results, scheduler progress, correlation IDs and usage accounting. Providers
and handlers are re-registered, not serialized. A version 1 file cannot contain
a runtime or scheduler continuation; the engine migrates its valid turn boundary
without inventing suspended work.

Both readable versions require the complete outer envelope, four typed
identity fields, structured turn and transcript entries, and non-negative
integer counters (`validate_payload`, `:248`). Unknown envelope, record or
legacy-loop fields and malformed values are errors rather than a source of
silent empty strings or reset counters. Standalone snapshots may have an empty
loop; optional legacy phase fields are type-checked when present. `runtime` and
`scheduler` are accepted only under version 2 (`:326`). Enforced by
`unparseable_json_names_the_file` (`:484`), which strips and mistypes each
envelope field in turn.

### Identity is checked before anything is applied

`identity_for` (`:74`) sorts participants because registration order is not
part of what makes two runs the same session; `check_identity` (`:95`) names
the first differing field, both values and the two ways out. Resume is
automatic, so this check is what stands between a stale file and a run that
silently inherits an unrelated conversation. Enforced by
`the_same_run_passes_however_it_was_registered` (`:625`) and
`a_mismatch_is_refused_named_and_recoverable` (`:643`), and from a whole run
by `crates/kerness/tests/resume.rs:276` and `:304`.

### The write is atomic and touches nothing it did not create

`save_snapshot` (`crates/kerness/src/sessionfile.rs:121`) exclusively creates a sibling temporary file with
`create_new`, taking a fresh process-and-counter suffix when the first name is
occupied (`:141`). Existing files and symlinks at that name are left untouched.
Bytes go through the opened handle; only a successful write and `sync_all` are
renamed over the destination. A write or rename failure removes the file this
save created and leaves the previous snapshot intact (`:176`). On Unix the
temporary file is created owner-only and the parent directory is synced after
the rename. A directory-sync failure is reported even though the new snapshot
has already replaced the old one; callers must not infer that a failed save
always left the old file in place. Missing parent directories are created
(`:122`). Enforced by
`a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else` (`:545`),
which covers permissions, a symlink squatting on the temporary name, an
occupied temporary name, a directory at the destination, and the leftover set.

### A missing file is the first run

`load_snapshot` (`crates/kerness/src/sessionfile.rs:183`) returns `Ok(None)` for an absent path and never
creates one — the same rule [memory.md](memory.md)'s `Memory::load` follows. A
file that exists but is not a snapshot fails naming the file. Enforced by
`a_missing_file_is_not_an_error_and_is_not_created` (`:452`) and
`an_unknown_schema_version_is_refused_naming_both` (`:465`).

### When the run writes

The owned run checkpoints at every durable boundary
([run.md](run.md)'s `checkpoint`, `crates/kerness/src/session/run.rs:360`):
before each event is delivered, after a turn begins, when an approval is
recorded, before a tool's side effect (intent) and after it (completion), and
at the terminal outcome. Without a configured `session_file` the call is a
no-op. The compatibility adapter writes through `Session::save`
(`crates/kerness/src/session.rs:1680`) after each delivered turn and reads
through `Session::resume` (`:1650`). No test in this module drives those
callers; `crates/kerness/tests/resume.rs:125`
(`the_state_is_written_after_every_turn`) and `:168`
(`no_temporary_file_is_left_behind`) do.

## Key Types and Entry Points

- `crates/kerness/src/sessionfile.rs:36` — `SCHEMA_VERSION` — `2`; the writer's
  version, and the newest the reader accepts alongside `1`.
- `crates/kerness/src/sessionfile.rs:47` — `SessionSnapshot` — identity,
  turns, transcript, `loop_state` (the `loop` key on disk) and `compactions`;
  `new(identity)` (`:61`) is a run that has said nothing yet.
- `crates/kerness/src/sessionfile.rs:74` — `identity_for(gameplan, topic,
  participants, orchestrator)` — the identity block, participants sorted.
- `crates/kerness/src/sessionfile.rs:95` — `check_identity(saved, current)` —
  `Ok(())` or `Error::Session` naming the first field that differs.
- `crates/kerness/src/sessionfile.rs:121` — `save_snapshot(path, snapshot)` —
  validate, write a private temporary sibling, sync, rename, sync the parent;
  `Error::Io` names the path that failed and leaves the previous file intact.
- `crates/kerness/src/sessionfile.rs:183` — `load_snapshot(path)` —
  `Result<Option<SessionSnapshot>>`: `None` for a missing file, `Error::Session`
  for bad JSON, a wrong version or a malformed envelope, `Error::Io` for an
  unreadable file.
- `crates/kerness/src/sessionfile.rs:248` — `validate_payload(payload, path)`
  — the shared envelope rule both `save` and `load` apply, so a snapshot the
  crate cannot read back is refused before it is written.
- `bindings/python/src/funcs.rs:572` — `PySessionSnapshot` — the pyclass;
  `__eq__` (`:645`) compares the inner record, which is what lets a Python test
  write `load_snapshot(path) == snapshot`.

## Interactions

- Written by [run.md](run.md) at explicit runtime boundaries when a session
  file is configured, including action intent before side effects and
  completion afterward; the `loop.runtime` object is that module's schema and
  the envelope is this one's. Integration: `crates/kerness/tests/resume.rs:422`,
  `:625`, `:771`.
- Read and written by [session.md](session.md)'s compatibility adapter
  (`resume`, `save`), which restores the conversation and hands `loop_state` to
  the scheduler. Integration: `crates/kerness/tests/resume.rs:219`.
- Holds the turns and transcript owned by [conversation.md](conversation.md),
  restored together through `Conversation::restore` because a snapshot holds
  both; the record shapes are that module's serde derives.
- Holds the state produced by [loop.md](loop.md)'s `snapshot()`
  (`crates/kerness/src/orchestrator.rs:711`): `turn_count`, `phases` and,
  once initialized, `scheduler`.
- Holds [provider.md](provider.md)'s usage ledger inside the version 2
  continuation.
- Records each provider exchange through [agent-runtime.md](agent-runtime.md)'s
  `with_record` hook (`crates/kerness/src/agent_runtime.rs:340`) when
  `tool_results_in_history` is set, and the compaction count that
  [compaction.md](compaction.md) increments
  (`crates/kerness/tests/compaction_e2e.rs:220`).
- The session file path is confined by [access.md](access.md) at
  `Session::new` (`crates/kerness/src/session.rs:579`); nothing here checks
  the path again.

## How to Test

```sh
cargo test -p kerness --lib sessionfile                                  # pass = 8 passed
cargo test -p kerness --test resume                                      # pass = 12 passed
.venv/bin/python -m pytest bindings/python/tests/test_sessionfile.py -q  # pass = 11 passed
```

Rebuild the extension (`cd bindings/python && ../../.venv/bin/maturin
develop`) before the Python command after a Rust change.

- `crates/kerness/src/sessionfile.rs:367` — `every_kind_of_record_survives` —
  typed record round trip, version 1 boundary reading, and version 2
  continuation preservation.
- `crates/kerness/src/sessionfile.rs:484` — `unparseable_json_names_the_file`
  — missing, mistyped, negative, unknown and version-incompatible envelope
  fields each reject naming the file.
- `crates/kerness/src/sessionfile.rs:545` —
  `a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else` — exclusive
  temporary creation, private permissions, symlink-collision safety,
  replacement, and cleanup on failure.
- `crates/kerness/src/sessionfile.rs:452`, `:465`, `:625`, `:643` — the
  missing-file rule, the version refusal, and both identity directions.
- `crates/kerness/tests/resume.rs:422` —
  `a_saved_native_approval_resumes_after_completed_tools_without_replaying_them`
  — repeated restore and checkpoint preserves prior committed turns, phase
  state, approval identity and native call progress; malformed counters and
  forged identities reject.
- `crates/kerness/tests/resume.rs:625` —
  `an_intent_without_completion_requires_reconciliation_and_never_replays_the_handler`
  — a captured intent requires reconciliation or cancellation, including a
  completion-save failure.
- `crates/kerness/tests/resume.rs:771` —
  `a_failed_intent_write_prevents_the_tool_side_effect` — an intent-save
  failure prevents execution.
- `crates/kerness/tests/resume.rs:143`, `:202`, `:276`, `:304`, `:331` — the
  file carries its version and identity; a missing file is a silent first run;
  a different run, a different roster and a non-snapshot are each refused with
  a reason.
- `bindings/python/tests/test_sessionfile.py:31` — `TestRoundTrip`; `:77`
  `TestMissingAndMalformed`; `:123` `TestIdentity` — the same properties seen
  through the boundary, including the `loop=` keyword and `__eq__`.
- Gap: the Unix-only permission and directory-sync branches are asserted on
  Unix only (`cfg(unix)` inside `crates/kerness/src/sessionfile.rs:545`); no CI runner exercises the
  `not(unix)` twin.

## Review and Refactor Guide

- Adding a field to the envelope → bump `SCHEMA_VERSION`
  (`crates/kerness/src/sessionfile.rs:36`), add it to
  the payload at `:129`, to `validate_payload`'s field list at `:255`, to the
  `SessionSnapshot` read at `:213`, and to `PySessionSnapshot`'s constructor
  and getters (`bindings/python/src/funcs.rs:582` onward); decide whether the
  previous version stays readable and extend `every_kind_of_record_survives`
  with a fixture for it.
- Adding a field inside `loop` → it belongs to [loop.md](loop.md) or
  [run.md](run.md); the only change here is the whitelist in
  `validate_payload` (`crates/kerness/src/sessionfile.rs:317` onward), which
  rejects an unknown `loop` key.
- Changing the write path → keep the sequence create-new, write, `sync_all`,
  rename, `sync_parent`, and the cleanup-only-what-we-created rule at `:176`;
  `a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else` asserts
  every branch.
- Changing an error message → the tests match `run.json` and the two-ways-out
  phrasing; `bindings/python/tests/test_sessionfile.py:85` and `:98` assert
  from Python.
- Safe extension points: a new identity field goes in `IDENTITY_FIELDS`
  (`crates/kerness/src/sessionfile.rs:43`) and `identity_for`, and its type check in `validate_payload`; the
  check reports fields in that array's order.
- Patterns to reuse: hand-built envelope plus `validate_payload` on both paths,
  rather than a `Deserialize` derive that would default a missing field.
- Forbidden coupling: no knowledge of the runtime or scheduler object shapes
  beyond "an object under version 2"; no access-policy check (the session
  confines the path once); no reading of the file by any module other than
  `session.rs` and `session/run.rs`.
- Compatibility checks: `SCHEMA_VERSION` is asserted by
  `crates/kerness/tests/public_api.rs:43` and exported through
  `bindings/python/kerness/sessionfile.py`; the Python constructor signature is
  `bindings/python/src/funcs.rs:582`.

Improvement candidates (proposals, not accepted work):

- Emit a warning through [channel.md](channel.md)'s logger when
  `sync_parent` fails after a successful rename, so the "replacement happened
  but was reported as failure" case is distinguishable in a log. Benefit: an
  operator can tell the two apart. Check: a unit test with a parent the process
  cannot open for sync observes the warning and the new file.
- Add a `not(unix)` CI check, or mark the Unix-only branches explicitly in
  `a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else`. Benefit:
  the platform gap is visible rather than silent. Check: the test's `cfg`
  block is documented in the Status line above.

## Open Gaps / Roadmap

- Saving rewrites the whole file each time; there is no append log.
- An intent without a completion is indeterminate after a crash. The run
  requires explicit host reconciliation instead of replaying an arbitrary
  external side effect; there is no exactly-once guarantee.
- File replacement is atomic on supported local filesystems. Durability still
  depends on filesystem and storage sync guarantees; directory sync is Unix
  only.
- The `loop` whitelist in `validate_payload`
  (`crates/kerness/src/sessionfile.rs:317` onward) names the legacy
  phase fields literally; a new scheduler field is rejected until added there,
  which is the intended failure but lands as a resume error rather than a
  compile error.
