# Agent Runtime

## Goal

One agent turn, from the provider call to the text that goes into the
transcript. The cycle is: call the provider, parse tool calls out of the reply,
dispatch them, feed the results back, and call again — until the model stops
asking for tools or a bound is hit.

This is the smallest piece of the framework a caller can reuse on its own: a
harness that wants a different loop but the same turn mechanics constructs an
`AgentRunner` directly.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent_runtime.rs` | `AgentRunner` and the tool-call cycle |
| `bindings/python/src/runtime.rs` | `PyAgentRunner` |
| `bindings/python/kerness/agent_runtime.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/agent_runtime.rs:61` — `AgentRunner<'a>` — borrows the
  agent, the dispatcher, and the callbacks; it is built for one turn and dropped.
- `crates/kerness/src/agent_runtime.rs:80` — `new(...)` — the required parts.
- `crates/kerness/src/agent_runtime.rs:129` — `run(...)` — the cycle itself.
- `crates/kerness/src/agent_runtime.rs:31` — `FOLLOWUP_PROMPT` — what the model is
  told after tool results are appended.
- `crates/kerness/src/agent_runtime.rs:41` — `MAX_INVALID_CALLS` — three
  consecutive unparseable tool blocks end the turn rather than looping.
- `crates/kerness/src/agent_runtime.rs:52` — `MAX_REPEATED_FAILURES` — three
  consecutive rounds in which every call failed with exactly the failures of the
  round before end the turn.
- `crates/kerness/src/agent_runtime.rs:101` — `with_max_tool_iterations(limit)` —
  the per-turn ceiling on tool rounds.
- `crates/kerness/src/agent_runtime.rs:108` — `with_record(f)` — the hook the
  session uses to write each provider exchange to the session file.

### Three bounds, and why the framework owns two of them

The loop is driven by the model rather than by a count, so it needs bounds a
stuck model cannot argue with. `max_tool_iterations` is the caller's and is
optional. The other two are the framework's and are not, because with the
iteration bound unset an unbounded loop runs against a paid API.

Both watch for the same thing — a round that told the model nothing it was not
told last round — at two different depths:

- `MAX_INVALID_CALLS` counts blocks that never parsed. An invalid block gets the
  same "here is the format" text every time, so a model that cannot produce
  valid JSON emits the same reply forever.
- `MAX_REPEATED_FAILURES` counts rounds that parsed and got nowhere: every call
  in the round failed, and the results are equal, in order, to the previous
  round's (`agent_runtime.rs:183`). The model has been told what is wrong, has
  changed nothing, and would be told the same thing again.

Both counters are *consecutive*, reset by any round that made progress. That is
what keeps a model that recovers from one bad block, or works through several
different wrong calls, from being cut off — a round with even one success in it
is progress, and the guard has to let it run.

Both end the turn, not the session, and both log a warning naming the purpose.
A turn that produced no text is a turn the session can carry on without, so this
is deliberately not an [errors.md](errors.md) error and deliberately not a
[loop.md](loop.md) end reason: the session's own bounds decide when a run stops.

## Interactions

- Called by [session.md](session.md) for a participant turn and by
  [loop.md](loop.md) for an orchestrator turn.
- Calls [provider.md](provider.md) for each model exchange.
- Parses calls with [toolkit.md](toolkit.md) and dispatches through its
  `ToolDispatcher`; `MAX_REPEATED_FAILURES` compares the `ToolResult`s that
  dispatcher returns, so an access refusal repeated verbatim trips it.
- Records exchanges into [sessionfile.md](sessionfile.md) via `with_record`.

## How to Test

```sh
cargo test -p kerness agent_runtime                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_agent_runtime.py -q # pass = 0 failed
```

- The Rust tests drive a `MockProvider` (`crates/kerness/src/agent_runtime.rs:339`)
  through a fixed reply sequence, so the cycle is exercised without a network call.
- `crates/kerness/src/agent_runtime.rs:550` —
  `a_model_repeating_one_failing_call_does_not_loop_forever` — the block parses,
  so `MAX_INVALID_CALLS` never sees it. `:570`
  `a_failing_call_the_model_varies_is_left_alone` and `:590`
  `a_tool_that_keeps_succeeding_is_never_cut_off` are the two the guard must not
  catch.
- `bindings/python/tests/test_agent_runtime.py:129` — `test_a_model_stuck_on_invalid_json_does_not_loop_forever` —
  the `MAX_INVALID_CALLS` cutoff — paired with `:152`
  `test_a_recovering_model_is_not_penalised_for_an_earlier_bad_block`, which is
  what makes the counter consecutive rather than cumulative.
- `:140` — `test_a_model_repeating_a_hopeless_call_does_not_loop_forever` — the
  `MAX_REPEATED_FAILURES` cutoff reached through a Python handler that raises,
  which is the part the crate's own tests cannot exercise. What the guard must
  *not* catch is counter logic, asserted in `agent_runtime.rs` alone.
- `:82` — `test_the_caller_history_is_not_mutated` — a turn appends to its own
  copy.
- `:284` — `test_a_fenced_call_still_works_under_a_native_dialect` — the prompt
  fallback is not switched off just because native tools are available.

## Open Gaps / Roadmap

- Tool calls within one reply are dispatched in order, one at a time. Parallel
  dispatch would change nothing observable today because everything is
  synchronous, but a caller with slow tools has no way to overlap them.
- All three bounds are per turn, not per session. A model that trips
  `MAX_REPEATED_FAILURES` every turn produces a short turn every turn and the
  session runs to its own limit; nothing aggregates the pattern. Budgets on the
  root roadmap are where a session-wide bound would live.
- `MAX_REPEATED_FAILURES` compares results for equality, so a failure carrying a
  varying detail — a timestamp, a path that changes — reads as progress and is
  never counted.
