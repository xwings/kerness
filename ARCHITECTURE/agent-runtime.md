# Agent Runtime

## Goal

One agent turn, from the provider call to the text that goes into the
transcript. The cycle is: call the provider, parse tool calls out of the reply,
dispatch them, feed the results back, and call again — until the model stops
asking for tools or the iteration limit is hit. Serves **M2**.

This is the smallest piece of the framework a caller can reuse on its own: a
harness that wants a different loop but the same turn mechanics constructs an
`AgentRunner` directly.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent_runtime.rs` | `AgentRunner` and the tool-call cycle |
| `crates/kerness-py/src/runtime.rs` | `PyAgentRunner` |
| `python/kerness/agent_runtime.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/agent_runtime.rs:42` — `AgentRunner<'a>` — borrows the
  agent, the dispatcher, and the callbacks; it is built for one turn and dropped.
- `crates/kerness/src/agent_runtime.rs:61` — `new(...)` — the required parts.
- `crates/kerness/src/agent_runtime.rs:110` — `run(...)` — the cycle itself.
- `crates/kerness/src/agent_runtime.rs:23` — `FOLLOWUP_PROMPT` — what the model is
  told after tool results are appended.
- `crates/kerness/src/agent_runtime.rs:33` — `MAX_INVALID_CALLS` — three
  consecutive unparseable tool blocks end the turn rather than looping.
- `crates/kerness/src/agent_runtime.rs:82` — `with_max_tool_iterations(limit)` —
  the per-turn ceiling on tool rounds.
- `crates/kerness/src/agent_runtime.rs:89` — `with_record(f)` — the hook the
  session uses to write each provider exchange to the session file.

`AgentRunner` borrows rather than owns, which no `#[pyclass]` can express. The
Python class therefore holds the pieces and builds the runner inside each call —
see [bindings.md](bindings.md).

## Interactions

- Called by [session.md](session.md) for a participant turn and by
  [loop.md](loop.md) for an orchestrator turn.
- Calls [provider.md](provider.md) for each model exchange.
- Parses calls with [toolkit.md](toolkit.md) and dispatches through its
  `ToolDispatcher`.
- Records exchanges into [sessionfile.md](sessionfile.md) via `with_record`.

## How to Test

```sh
cargo test -p kerness agent_runtime                        # pass = 0 failed
.venv/bin/python -m pytest tests/test_agent_runtime.py -q  # pass = 0 failed
```

- The Rust tests drive a `MockProvider` (`crates/kerness/src/agent_runtime.rs:290`)
  through a fixed reply sequence, so the cycle is exercised without a network call.
- `tests/test_agent_runtime.py:128` — `test_a_model_stuck_on_invalid_json_does_not_loop_forever` —
  the `MAX_INVALID_CALLS` cutoff — paired with `:139`
  `test_a_recovering_model_is_not_penalised_for_an_earlier_bad_block`, which is
  what makes the counter consecutive rather than cumulative.
- `:81` — `test_the_caller_history_is_not_mutated` — a turn appends to its own
  copy.
- `:246` — `test_a_fenced_call_still_works_under_a_native_dialect` — the prompt
  fallback is not switched off just because native tools are available.

## Open Gaps / Roadmap

- Tool calls within one reply are dispatched in order, one at a time. Parallel
  dispatch would change nothing observable today because everything is
  synchronous, but a caller with slow tools has no way to overlap them.
- The iteration limit is per turn, not per session; a model that asks for the
  ceiling every turn is not detected.
