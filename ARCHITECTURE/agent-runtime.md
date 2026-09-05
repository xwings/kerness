# Agent Runtime

## Goal

Run one agent turn through provider requests and ordered tool results. The
continuation is owned data, so a host can inspect the next call, request
approval, persist it, and resume without repeating a completed tool. The
blocking `AgentRunner::run` and caller-driven sessions use the same state.
This implements M2's turn stepping and M3's typed turn outcomes.

## Status

`done` — owned continuations, typed turn reasons, and legacy driving are
implemented; the Rust and rebuilt Python module tests pass.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent_runtime.rs` | owned turn state, provider advancement, legacy driver |
| `bindings/python/src/runtime.rs` | standalone `PyAgentRunner` adapter |
| `bindings/python/kerness/agent_runtime.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/agent_runtime.rs:51` — `AgentTurn` — private scratch,
  pending calls, completed results, loop guards, and record outbox; serializable
  without carrying a provider or a handler.
- `crates/kerness/src/agent_runtime.rs:154` — `snapshot` / `:158` `from_snapshot` — preserve the
  exact tool cursor and reject inconsistent history or result positions.
- `crates/kerness/src/agent_runtime.rs:243` — `accept_tool_result` — consumes one pending call;
  `pending_call` exposes it without executing it.
- `crates/kerness/src/agent_runtime.rs:140` — `take_recorded` — drains newly appended exchange
  messages once; the drained state survives a snapshot.
- `crates/kerness/src/agent_runtime.rs:146` — `replace_history` — replaces only the shared-history
  prefix after compaction, preserving the instruction and private tool results.
- `crates/kerness/src/agent_runtime.rs:293` — `AgentRunner` — borrows the provider, agent,
  dispatcher, and prompt callbacks while advancing an owned continuation.
- `crates/kerness/src/agent_runtime.rs:356` — `start` — assembles initial state without IO;
  `:376` `advance` makes one logical provider request and executes no tools.
- `crates/kerness/src/agent_runtime.rs:368` — `with_strict_errors` — returns provider errors to a
  session driver; the default records their cause and returns a placeholder.
- `crates/kerness/src/agent_runtime.rs:410` — `run` — drives the same state to completion and
  dispatches each pending call through `ToolDispatcher`.
- `crates/kerness/src/agent_runtime.rs:39` — `TurnReason`; `:134` `reason` —
  distinguish a completed answer from an iteration limit, invalid calls,
  repeated failures, or an absorbed provider error.

### What a step promises

`advance` calls `Provider::chat_with_retries` once, preserving provider
subclasses that override that method. Its internal retries can make several
network requests; this is a logical provider boundary, not a network-attempt
boundary. Usage observers count the attempts the provider exposes.

A provider response can queue several tools. Each `accept_tool_result` commits
one result and advances one cursor. Advancing the provider while any call is
pending is an error. Native assistant/tool messages therefore reach the next
provider request as a complete batch, even if a session was saved between two
calls. Only the final answer belongs to the shared conversation by default.

`advance` propagates context overflow with the continuation unchanged.
Compaction replaces the shared-history prefix and retries the provider with
completed tool results still present. A standalone legacy `run` cannot expose
its private continuation to its caller, so it preserves its placeholder on a
followup failure; an opening overflow still propagates.

### Bounds and record delivery

`max_tool_iterations` caps tool rounds. `MAX_INVALID_CALLS` stops three
consecutive unparseable blocks; `MAX_REPEATED_FAILURES` stops three repeats of
an identical all-error result batch. Their counters and previous results are
part of the continuation, so restoring a snapshot grants no new allowance.
A success or a different failure resets the relevant consecutive counter.
Restoration rejects impossible guard counts and terminal state without its
reason. Public mutation validates even states deserialized directly through
serde; saturating counters cannot wrap back into a fresh allowance.

The record outbox holds exchanges until `take_recorded` consumes them. A caller
that persists those exchanges must checkpoint the drained continuation with
the corresponding conversation update. The standalone `with_record` callback
consumes the same outbox. Final text is returned separately.

## Interactions

- [session.md](session.md) binds providers, prompt assembly, permissions,
  approval, persistence, cancellation, and usage budgets around each step.
- [loop.md](loop.md) requests whole turns without knowing how many provider or
  tool steps they require.
- [provider.md](provider.md) owns retries and wire transport.
- [toolkit.md](toolkit.md) validates and dispatches the single pending call;
  [toolschema.md](toolschema.md) renders its dialect-specific result.
- [sessionfile.md](sessionfile.md) persists turn state together with loop state.

## How to Test

```sh
cargo test -p kerness --lib agent_runtime
.venv/bin/python -m pytest bindings/python/tests/test_agent_runtime.py -q
```

Both commands must exit zero. Existing native-exchange coverage exercises two
calls, restores between them, and checks correlation IDs and exactly two tool
executions. Multi-round and guard tests restore after each step. Recording tests
prove a drained outbox stays drained, and the followup-failure test proves an
overflow retry preserves its instruction and completed tool output. The legacy
provider fixture overrides `chat_with_retries` and refuses direct `chat` calls.
Guard owners assert typed completion reasons after restoration and refuse
malformed counters before a provider or result mutation can run.
Verified on the current source: 19 Rust agent-runtime tests pass; the rebuilt
Python agent-runtime and loop suites pass together with 57 tests.

## Open Gaps / Roadmap

- One logical provider request may block through provider-owned retries.
  Cancellation is cooperative between runtime steps.
- Tool handlers execute synchronously; an arbitrary handler needs its own
  deadline or cancellation support while it is running.
- Repeated-failure detection compares rendered results. A changing timestamp
  or other varying detail counts as a different failure.
