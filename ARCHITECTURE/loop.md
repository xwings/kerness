# Orchestrator Loop

## Goal

When a gameplan declares an orchestrator, the orchestrator runs the session: it
decides who speaks next, tracks which phase the harness is in, watches for the
termination keyword, and at the end produces the named result fields the contract
demands. This module is that loop, and it is separable — a harness that wants a
different control flow implements `LoopHost` and keeps everything else. Serves
**M2**.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/orchestrator.rs` | `LoopHost`, `LoopState`, `PhaseTracker`, `OrchestratorLoop`, result parsing |
| `crates/kerness-py/src/runtime.rs:499,619` | `PyLoopState`, `PyOrchestratorLoop` |
| `python/kerness/loop.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/orchestrator.rs:47` — `LoopHost` — what the loop needs from
  whatever is running it: run an agent turn, deliver a message, read the roster.
  `Session` implements it at `session.rs:1208`.
- `crates/kerness/src/orchestrator.rs:381` — `OrchestratorLoop` — the loop itself,
  built with `new` (`:400`) and the `with_*` options at `:420`–`:438`.
- `crates/kerness/src/orchestrator.rs:447` — `run(host)` — one call, returns the
  final `LoopState`.
- `crates/kerness/src/orchestrator.rs:114` — `LoopState` — round, phase, turns
  taken, end reason, and the accumulated result fields.
- `crates/kerness/src/orchestrator.rs:84` — `EndReason` — why the loop stopped:
  consensus, exhausted rounds, forced end, or an explicit terminator.
- `crates/kerness/src/orchestrator.rs:179` — `PhaseTracker` — advances through the
  declared phases and decides when each is satisfied.
- `crates/kerness/src/orchestrator.rs:473` — `snapshot()` — the resumable state,
  handed to [sessionfile.md](sessionfile.md).
- `crates/kerness/src/orchestrator.rs:704` — `closing_prompt(fields)` — asks the
  orchestrator for the result block; `:734` `verdict_rethink_prompt` asks it to
  reconsider a draft.
- `crates/kerness/src/orchestrator.rs:761` — `parse_result_fields(text, fields)` —
  pulls the declared fields out of the reply; `:780` `strip_result_block` removes
  that block from what goes into the transcript.
- `crates/kerness/src/orchestrator.rs:29` — `FORCED_END_NOTE` — what the transcript
  records when the orchestrator's output stayed unparseable through its retries.

`LoopHost` and `PhaseTracker` are not exported from the Python package: the first
is a trait a Rust caller implements, and the second is internal to the loop. The
loop, its state, and the end reasons are exported.

## Interactions

- Driven by [session.md](session.md), which is also its `LoopHost`.
- Runs each turn through [agent-runtime.md](agent-runtime.md).
- Its phases, rounds, terminators, and result fields come from
  [harness.md](harness.md).
- Scans replies with [utils.md](utils.md)'s `keyword_in_text` and
  `parse_orchestrator_call`.
- Its snapshot is written by [sessionfile.md](sessionfile.md).

## How to Test

```sh
cargo test -p kerness orchestrator                # pass = 0 failed
.venv/bin/python -m pytest tests/test_loop.py -q  # pass = 0 failed
```

- The Rust tests drive a `StubHost` (`orchestrator.rs:909`) through fixed
  orchestrator replies, which is how phase advance, forced end, and each
  `EndReason` are covered without a provider.

## Open Gaps / Roadmap

- The orchestrator addresses one participant per turn. A gameplan that wants two
  to answer the same question in parallel has to sequence them.
- `parse_result_fields` matches on the declared field names in the reply text; a
  model that renames a field produces a missing field rather than a mismatch
  error.
- Phase transitions are forward-only. There is no way for an orchestrator to
  return to an earlier phase.
