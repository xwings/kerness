---
eatmycode_version: "2.0.0"
---

# Memory, conversation and persistence

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing memory stores/filtering, transcript representation, context
compaction, snapshot schema, atomic writes or persisted resume compatibility.

## Responsibility and Status

Implemented: shared notes outlive runs; structured conversation renders for each
provider; optional session snapshots retain resumable state. Compaction bounds
working history heuristically. This owner covers representations/storage, while
runtime owns live continuations and when checkpoints occur. Offline tests cover
failure/recovery contracts, not multi-process storage coordination.

## Code Map

| Path / symbol | Role |
| --- | --- |
| [memory.rs](../../crates/kerness/src/memory.rs), `MemoryStore` | File primitive, FileMemory, SummarizingMemory, CuratedMemory, scope/filter/lifecycle contracts. |
| [conversation.rs](../../crates/kerness/src/conversation.rs), `Conversation` | Structured turns, rendered provider messages and separate public transcript. |
| [compaction.rs](../../crates/kerness/src/compaction.rs), `compact` | Character estimate and summary replacement of old turns. |
| [sessionfile.rs](../../crates/kerness/src/sessionfile.rs), `save_snapshot`, `load_snapshot` | Versioned validation, migration, atomic writes and identity checks. |
| [compaction_e2e.rs](../../crates/kerness/tests/compaction_e2e.rs), [resume.rs](../../crates/kerness/tests/resume.rs), [session_run.rs](../../crates/kerness/tests/session_run.rs) | Context limits and persisted continuation through public sessions. |

## Local Conventions

Use [root conventions](../../ARCHITECTURE.md#code-conventions). Memory scopes are
opaque to the session: only a store interprets a scope as a path/key. Stores expose
their file path when confinement applies; clone resource handles before host
callbacks. Persistence errors must identify the file and preserve useful prior
state. Keep serialized schema validation explicit rather than accepting unknown
or contradictory continuation data silently.

## Contracts and Invariants

- `MemoryStore` requires `read`/`append`, with optional lifecycle/curation/path/age
  methods. FileMemory treats the scope as a path and does not create files on
  read. SummarizingMemory bounds entries and folds overflow on maintenance;
  CuratedMemory enforces a character ceiling and asks agents to curate. Keyed
  stores percent-encode scope bytes into collision-free filenames (`memory.rs`).
- Session memory writes are opt-in. Agent notes from the tool and `@MEMORY:`
  marker pass through the host filter; direct host writes are outside that
  untrusted input path. The runtime quotes notes and reports age in prompts.
  `MemoryStore::path` opts disk-backed stores into setup confinement; a custom
  store's undisclosed external IO remains a host responsibility.
- Conversation keeps `Turn` fields separate from provider rendering. Public
  transcript and working history differ deliberately: directives belong to model
  history, system notices may belong only to caller output (`conversation.rs`).
- `compact` estimates Unicode characters at four per token; it keeps the topic
  and newest turns and replaces dropped material with a labelled summary.
  No useful summary means no replacement. The runtime subtracts prompts/tools/
  memory from the available window and handles reactive overflow retry
  (`compaction.rs`, `session.rs`, `compaction_e2e.rs`). This is not billing.
- Snapshot schema is version 2; valid version 1 turn-boundary files migrate.
  Absent file means first run; malformed/version/identity mismatches fail instead
  of silently starting fresh (`sessionfile.rs`, `resume.rs`). No configured file
  means no persistence.
- Saves exclusively create a sibling temporary file, sync it, rename it, then
  sync the directory on Unix. Snapshot files use owner-only `0600` permissions
  on Unix: saved prompts/tool arguments are private host-owned state. Existing
  temporary files/symlinks are untouched;
  failure removes only this save's temporary. Directory-sync failure is reported
  even though the rename already happened (`save_snapshot` inline tests).
- Snapshots store data, never callbacks/providers. Runtime rebinds resources and
  validates the saved contract. Pending approvals keep identity/completed results;
  uncertain interrupted effects need reconciliation before further execution
  (`session/run.rs`, `session_run.rs`).

## Dependencies and Boundaries

Read [runtime](runtime.md) for checkpoint timing, continuation shape, prepared
prompts or memory maintenance; it owns `session/run.rs` and invocation state.
Read [tools/access](tools-access.md) when file scope, memory grants/filtering or
model-write paths change. Read [providers](providers.md) for summarizer calls,
context-overflow classification and usage accounting. Read
[Python bindings](python-bindings.md) when store methods, record shapes or errors
change. Storage does not choose scheduling or authorize arbitrary host IO.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| Store/filter/curation/lifecycle | `memory.rs`, runtime call sites; inline store tests | Read runtime/tools-access and bindings; verify no writes on read, filter coverage and scoped paths. |
| Turn render or compaction | `conversation.rs`, `compaction.rs`, runtime limits; `compaction_e2e.rs` | Read runtime/providers; preserve topic/newest turns, transcript and failure fallback. |
| Schema/save/resume | `sessionfile.rs`, runtime snapshots; `resume.rs`, `session_run.rs` | Read runtime; validate old schema, mismatched identity, pending effects and atomic-write failure cases. |

## Verification

Use the [root Rust suite](../../ARCHITECTURE.md#verification): inline store,
conversation, compaction and snapshot tests plus `compaction_e2e`, `resume` and
`session_run` cover the contracts above. Use the
[Python suite](python-bindings.md#verification) for `test_memory.py`,
`test_conversation.py`, `test_compaction.py`, `test_sessionfile.py` and session
resume/approval tests. The runtime's offline `resume_approval` example exercises
durable rebinding. See [baseline evidence](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

Character estimates are language/model dependent. A too-large topic/latest turn
can remain over budget; no exact tokenizer is supplied. Arbitrary blocking store
callbacks are not forcibly interruptible. File snapshots are not a transactional
database or a declared multi-process writer-coordination protocol.
