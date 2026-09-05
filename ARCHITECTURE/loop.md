# Orchestrator Loop

## Goal

Own the harness's routing, phase progress, retry allowance, and closing verdict
as a resumable action machine. A session executes the requested action; the loop
itself performs no provider calls, filesystem operations, or channel writes.
This supplies M2's scheduling and host-driven phase progression.

## Status

`done` — action scheduling, host-driven phase reuse, and continuation restore
are implemented; Rust and rebuilt Python loop/resume tests pass.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/orchestrator.rs` | actions, loop state, phase tracker, compatibility driver, result parsing |
| `bindings/python/src/runtime.rs` | standalone loop and state adapters |
| `bindings/python/kerness/loop.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/orchestrator.rs:49` — `LoopHost` — the blocking adapter's
  provider-turn and delivery callbacks.
- `crates/kerness/src/orchestrator.rs:91` — `EndReason`; `:121` `LoopState` — limits, consensus,
  phase progress, and the accumulated result.
- `crates/kerness/src/orchestrator.rs:379` — `LoopTurnKind`; `:388` `LoopAction` — a requested
  turn, delivery, directive, note, summary, or completed state.
- `crates/kerness/src/orchestrator.rs:436` — `OrchestratorLoop` — owns phase state and its queued
  callbacks; no callback closures or providers appear in its snapshot.
- `crates/kerness/src/orchestrator.rs:567` — `next_action` — exposes a stable next action without
  consuming it; `:613` `submit_reply` consumes a requested agent turn.
- `crates/kerness/src/orchestrator.rs:649` — `acknowledge` — consumes a queued callback before the
  caller applies its effect and persists the resulting transcript.
- `crates/kerness/src/orchestrator.rs:711` — `snapshot` — version-1 counters plus scheduler stage,
  pending callbacks, full outcome, and any closing draft.
- `crates/kerness/src/orchestrator.rs:510` — `run` — drives those same actions through `LoopHost`.
- `crates/kerness/src/orchestrator.rs:679` — `commit_host_turn` — validates and accounts for a
  participant the host chose; `:700` `host_limit_reached` checks its bounds.
- `crates/kerness/src/orchestrator.rs:664` — `raw_closing_result` — preserves the final uncoerced
  reply for the session's strict result validation.

### Scheduling and checkpoints

An orchestrator response updates turn count, termination, and explicit phase
advances before queuing its delivery. A participant response updates the pending
set, round count, and phase before queuing its delivery too. The next participant
request is already selected in the saved scheduler state; restoring it does not
ask the orchestrator to route that completed response again.

`acknowledge` consumes a callback before the host applies it. The blocking
adapter publishes that consumed state through `record_position` before calling
`deliver`, so a host that saves inside delivery writes the matching transcript
and loop position. A session driver similarly checkpoints its conversation and
consumed callback together. External channel delivery is not a transaction with
that checkpoint.

A closing draft is stored before a rethink is requested. Only the final pass
queues a summary and supplies result fields. The complete state stays terminal
under explicit stepping. The legacy blocking adapter permits a fully completed
saved run to continue with a larger configured budget, while interrupted
continuations keep their exact pending action. Snapshots with only version-1
`turn_count` and `phases` use the original turn-boundary continuation.
Restoration rejects negative or inconsistent progress and active turns already
past their configured allowance. Saturating counters protect old hand-edited
phase snapshots from arithmetic overflow.
An immediate snapshot of a restored loop preserves the supplied continuation
without stepping. `next_action` or `host_limit_reached` validates and initializes
the deferred state before the caller inspects `state`. Successful initialization
releases the temporary resume map, leaving the owned scheduler state.

### Phase reuse without an automatic orchestrator

A host-driven session uses `host_instruction`, `host_briefing`,
`commit_host_turn`, and `host_limit_reached`. These methods use the same phase
tracker and turn ceiling but generate no automatic turns or callbacks. Unknown
participants and turns past the bound are errors. The session's configured mode
decides which driving interface it uses.

Every scheduled orchestrator turn receives the current pending roster, and every
participant receives the current phase requirement. Boundary directives are
additional shared context; they do not substitute for the per-turn briefing.
Repeated calls on an already-heard participant do not close a round while another
participant is still owed a turn.

## Interactions

- [session.md](session.md) executes actions, owns conversation and channels,
  validates final fields, and exposes partial outcomes.
- [agent-runtime.md](agent-runtime.md) turns each requested agent turn into
  provider and tool steps.
- [harness.md](harness.md) supplies limits, phases, terminators, and result fields.
- [sessionfile.md](sessionfile.md) persists scheduler state with conversation
  and any active agent continuation.
- [utils.md](utils.md) parses routing mentions and termination keywords.

## How to Test

```sh
cargo test -p kerness --lib orchestrator
.venv/bin/python -m pytest bindings/python/tests/test_loop.py -q
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q -k TestResumingFromASessionFile
```

All commands must exit zero. Existing phase tests assert progress at delivery,
explicit advancement during retries, and repeated-participant behavior. Resume
coverage checks both legacy completed-run extension and an exact pending
participant action. Closing coverage restores between draft and revision and
requires only the remaining closing call. The phase-round owner also drives the
same counters directly from a host, including unknown-agent refusal and bounds.
The resume owner rejects corrupt counters and preserves a restore/save cycle
performed before the first action.
Verified on the current source: 50 Rust loop tests pass; the rebuilt Python
agent-runtime and loop suites pass together with 57 tests, and all 6 selected
session-file resume tests pass.

## Open Gaps / Roadmap

- The orchestrator addresses one participant at a time; host-driven sessions can
  choose another schedule but tool/provider execution remains synchronous.
- Phase transitions are forward-only.
- `parse_result_fields` remains the legacy coercing parser. Strict validation
  belongs to the session outcome layer and uses `raw_closing_result`.
