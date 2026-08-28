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
| `bindings/python/src/runtime.rs:499,619` | `PyLoopState`, `PyOrchestratorLoop` |
| `bindings/python/kerness/loop.py` | re-export shim |

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
- `crates/kerness/src/orchestrator.rs:249` — `PhaseTracker::briefing()` — the
  active phase, the round, and who has yet to speak in it.
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

### Where the orchestrator learns who still owes a turn

The pending set lives in `PhaseTracker` and the orchestrator cannot see it, so
it is told — twice, for two different readers:

- **Every orchestrator turn** carries `standing_briefing()` (`:534`) as its
  `instruction`, the same per-turn channel `turn_instruction()` (`:704`) uses to
  put the phase requirement in front of every participant. This is how the
  orchestrator knows who to call.
- **Each phase boundary** additionally appends `brief()` (`:499`) to the shared
  conversation, so participants see the phase turn over.
- **Each retry** re-asks through `hint()` (`:711`), which names the head of the
  pending set outright. The generic "reply with an @Name" is the question the
  orchestrator has just demonstrated it cannot answer, so asking it again
  unchanged tends to draw the same unusable reply until the budget is spent and
  the session is forced to end — with the round one turn from closing.

The per-turn copy is what makes the rotation work, and a boundary-only briefing
is not a weaker version of it but a broken one. Between boundaries the pending
set shrinks with every participant who speaks, so a boundary-old copy names
people who have already spoken. An orchestrator that believes it re-calls one of
them; `record_turn` (`:284`) only removes a name that is still pending, so the
re-call clears nothing, the round never closes, and the next boundary — the only
thing that would have corrected the briefing — never arrives. The failure
sustains itself, and an orchestrator with a roster it cannot make progress
against will eventually stop calling participants and write their contributions
itself. For the same reason the orchestrator's rules block drops the "you
control the flow ... summarize at any point" licence when the harness declares
phases; see [session.md](session.md).

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
cargo test -p kerness orchestrator                               # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_loop.py -q # pass = 0 failed
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
