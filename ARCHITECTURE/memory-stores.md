---
eatmycode_version: "1.2.0"
---

# Memory store maintenance and curation

Supporting page for [memory.md](memory.md): how `SummarizingMemory` and
`CuratedMemory` manage scope growth. Summarization reduces old entries during
maintenance without a hard prompt-size bound; curation limits accepted note
characters. The store trait, filter, scopes and Python surface stay in the owner.

## SummarizingMemory

The second bundled store, and the reason the slot is worth having: notes that
only ever grow eventually cost more of every prompt than they are worth. It
keeps one JSON file per scope under a root (`Scope`,
`crates/kerness/src/memory.rs:486`), holding a running
summary and the entries written since that summary was last rewritten. `read`
renders the summary — labelled `CONSOLIDATED_PREFIX` (`:447`), so an agent can
tell a framework-written recap from a note somebody wrote — and then the
entries (`:526`). `append` writes through on every note (`:634`), so a crash
mid-run loses nothing that was committed.

The run asks `maintenance_scopes()` (`crates/kerness/src/memory.rs:667`) for
sorted overflowing scopes
after a successful result (`crates/kerness/src/session/run.rs:1150`).
`maintain_scope(scope)` (`crates/kerness/src/memory.rs:682`) consolidates exactly one scope per runtime
step: one logical provider operation carrying the running summary and entries
beyond `with_keep(entries)` (`:563`). The call goes through
`observe_provider_call` (`:602`), so it participates in
[provider.md](provider.md)'s run ledger and budget checks, including default
retries or an explicitly opaque custom override. A cancelled, failed, or
abandoned run skips this paid maintenance and keeps its written notes.
Standalone `close()` (`:660`) drives the same per-scope methods to completion.

Cleanup calls `close_run()` (`crates/kerness/src/memory.rs:225`). Its default
delegates to `close()`; `SummarizingMemory` overrides it with no work (`:705`)
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
before calling a provider (`crates/kerness/src/memory.rs:682`). A provider can
read memory during its callback; notes appended during consolidation remain
after the summarized prefix (`:696`).

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

## CuratedMemory

The third bundled store, and the other answer to the same problem: a scope is
held to `budget()` characters — `DEFAULT_MEMORY_BUDGET`
(`crates/kerness/src/memory.rs:716`) is 2,200,
roughly 550 tokens at [compaction.md](compaction.md)'s `CHARS_PER_TOKEN` — and
the agents are the ones who keep it under. One Markdown file per scope under a
root, entries joined by `ENTRY_SEPARATOR` (`:725`) on lines of their own, so a
scope stays a file somebody can read and hand-edit.

Four decisions carry the design:

- **It does not compact.** An append that would cross the ceiling is an
  `Error::Value` (`full`, `crates/kerness/src/memory.rs:838`) carrying the figure it would have reached
  and the entries as they stand, telling the writer to merge or remove and
  write again. The agent is mid-turn and has the tool to do it, and the
  alternative — dropping the oldest note to make room — discards the caller's
  material on a guess about which note mattered least.
- **An entry is addressed by a fragment of itself.** `revise`
  (`crates/kerness/src/memory.rs:922`) takes
  any substring appearing in exactly one entry; `locate` (`:855`) refuses a
  fragment matching none or several and names which, because rewriting a
  guess is the one failure the writer cannot detect. The replacement replaces
  the whole entry, not the fragment, so a revision is never a blind splice.
- **`read` leads with the usage line** (`crates/kerness/src/memory.rs:886`)
  — characters used, the
  ceiling, and the entry count — because an agent that cannot see how full
  the scope is cannot be asked to make room in it. An empty scope reads as
  the empty string, so `memory_block` renders nothing at all rather than
  `0 of 2,200`.
- **An exact duplicate is accepted and not stored twice**
  (`crates/kerness/src/memory.rs:901`). A model
  re-writing a note it already wrote has made no mistake worth an error, and
  spending the ceiling on a second copy is the outcome nobody wants.

Answering `budget()` (`crates/kerness/src/memory.rs:962`) is also what makes
the session offer the `edit_memory` tool
(`crates/kerness/src/session.rs:2160`). The gate is deliberate: a store that
keeps notes append-only takes the trait's `revise` default, which refuses
(`crates/kerness/src/memory.rs:265`), and advertising a tool whose every call would
be refused is worse than not offering it.
