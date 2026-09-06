---
eatmycode_version: "1.1.0"
---

# Agent Runtime

## Goal

Run one agent turn through provider requests and ordered tool results. The
continuation is owned data, so a host can inspect the next call, request
approval, persist it, and resume without repeating a completed tool. The
blocking `AgentRunner::run` and the owned run engine drive the same state.
This is M2's turn stepping and M3's typed turn outcomes.

The loop runs in a private scratch buffer seeded from the shared conversation,
and only the final text goes back into the conversation. That isolation is the
point: the buffer holds tool exchanges in one provider's message shape, and no
other agent should have to read them.

This module does not execute tools, choose which agent speaks, meter a call,
approve an action, or write a checkpoint: [toolkit.md](toolkit.md) dispatches,
[loop.md](loop.md) schedules, [provider.md](provider.md) meters, and
[run.md](run.md) approves and persists. It exposes the pending call and the
drained outbox so those owners can do their work between steps.

## Status

`done`. `cargo test -p kerness --lib agent_runtime` passes 19 tests, every
tool-loop case restoring the turn from its own snapshot between steps;
`.venv/bin/python -m pytest bindings/python/tests/test_agent_runtime.py -q`
passes 17; `cargo test -p kerness --test tools_e2e` passes 18 through a whole
session in all three dialects.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent_runtime.rs` | `AgentTurn` (owned continuation), `AgentRunner` (borrowing driver), the three constants, `TurnReason` |
| `bindings/python/src/runtime.rs:390` | `PyAgentRunner`, the standalone adapter that rebuilds the borrowing runner per call |
| `bindings/python/kerness/agent_runtime.py` | re-export shim: `AgentRunner` and the three constants |

## Language and Conventions

One Rust crate module and one pyclass in the binding's `runtime.rs`; a shim in
Python. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- **`AgentTurn` is `#[serde(deny_unknown_fields)]`**
  (`crates/kerness/src/agent_runtime.rs:50`), the crate-wide rule for every
  checkpointed shape: a field added to a saved turn is a schema change, not a
  silent default. `TurnReason` serialises `snake_case` (`:37`). Enforced by
  serde at `from_snapshot`.
- **Every public mutation validates first.** `accept_tool_result`,
  `replace_history`, `advance`, and `from_snapshot` each call `validate()`
  (`crates/kerness/src/agent_runtime.rs:165`), so a turn deserialised straight through serde is still checked
  ahead of any provider call. Observed in every `pub fn` that takes
  `&mut self`.
- **Counters saturate.** `iterations`, `invalid`, and `repeated` use
  `saturating_add`, so a hand-edited snapshot cannot wrap into a fresh
  allowance (`crates/kerness/src/agent_runtime.rs:207`, `:265`).
- **Borrowing driver, owned state.** `AgentRunner<'a>` (`crates/kerness/src/agent_runtime.rs:293`) borrows the
  agent, provider, and dispatcher and boxes its two callbacks; `AgentTurn`
  holds no reference. The Python class therefore stores the pieces and
  constructs the runner inside each `run` (`bindings/python/src/runtime.rs:474`),
  the pattern [bindings.md](bindings.md) names.
- **The pyclass constructor is keyword-only** and carries
  `#[allow(clippy::too_many_arguments)]` (`bindings/python/src/runtime.rs:417`);
  the Python `record_tool_exchange` and `tools_for` callbacks run under the
  parked-exception pattern so a raising sink still yields the turn.
- **Tests.** The unit suite drives every tool-loop case through
  `step_to_completion` (`crates/kerness/src/agent_runtime.rs:639`), which
  round-trips the turn through `snapshot`/`from_snapshot` after every step and
  asserts the `TurnReason`; its `MockProvider` (`:553`) overrides
  `chat_with_retries` (`:617`) so a runner that bypassed the logical boundary
  would fail. The Python suite's `runner(...)` helper
  (`bindings/python/tests/test_agent_runtime.py:36`) and `NativeMockProvider`
  (`:52`) play the same roles.

## Design and Invariants

### Decomposition and dependency direction

`agent_runtime` depends on `agent`, `error`, `logging`, `provider`, `tooling`,
`toolkit`, `toolschema`, and `usage`. Nothing below it imports it; above it,
[run.md](run.md) holds an `Option<AgentTurn>` as live state
(`crates/kerness/src/session/run.rs:219`), builds a strict runner per step
(`:800`, `:809`) and calls `advance` inside a usage scope (`:813`), commits
results through `accept_tool_result` (`:1015`), and drains the outbox in
`record_exchanges` (`:1034`). [session.md](session.md)'s blocking path builds
the legacy runner at `crates/kerness/src/session.rs:1193`.

Two halves. `AgentTurn` (`crates/kerness/src/agent_runtime.rs:51`) is the continuation: scratch messages, the
shared-history length, the pending call cursor, completed results, the guard
counters, the final text and its reason, and a record outbox. `AgentRunner`
(`:293`) is the driver: it can `start` a turn without IO, `advance` it by one
logical provider request, or `run` it to completion by dispatching each pending
call itself.

### What a step promises

`advance` (`crates/kerness/src/agent_runtime.rs:376`) calls `Provider::chat_with_retries` once, through the usage
observer (`chat`, `:445`), preserving provider subclasses that override that
method. Its internal retries can make several network requests; this is a
logical provider boundary, not a network-attempt boundary. Usage observers
count the attempts the provider exposes ([provider.md](provider.md)).

A provider response can queue several tools. Each `accept_tool_result` (`crates/kerness/src/agent_runtime.rs:243`)
commits one result and advances one cursor; advancing the provider while any
call is pending is an error. Native assistant/tool messages therefore reach the
next provider request as a complete batch, even if a session was saved between
two calls. Only the final answer belongs to the shared conversation by default.

### Invariants a change must preserve

- **Tool exchanges are private unless the session opts in.** Scratch messages
  reach the shared history only through the outbox and a `with_record` sink
  (`crates/kerness/src/agent_runtime.rs:340`); [run.md](run.md) forwards them when `tool_results_in_history` is
  set. Tests: `tool_exchanges_stay_private_to_the_turn_that_made_them`
  (`crates/kerness/tests/tools_e2e.rs:486`),
  `tool_results_in_history_shows_the_exchange_to_the_next_agent` (`:515`),
  `recording_captures_the_whole_exchange_when_asked`
  (`crates/kerness/src/agent_runtime.rs:914`).
- **A drained outbox stays drained.** `take_recorded` (`crates/kerness/src/agent_runtime.rs:140`) is
  `mem::take`, so a snapshot after draining cannot emit the same records twice.
  A caller that persists those exchanges must checkpoint the drained
  continuation together with the conversation update. Test: `:914`.
- **Restoration grants no new allowance.** The guard counters and the previous
  result batch are part of the continuation; `validate` (`crates/kerness/src/agent_runtime.rs:165`) rejects
  impossible counts, a terminal turn without its reason, a pending call past a
  hit iteration limit, and a results list that does not answer the calls list.
  Tests: every case driven through `step_to_completion` (`:639`), which restores
  after each step; `a_model_stuck_on_invalid_json_does_not_loop_forever`
  (`:774`), `a_model_repeating_one_failing_call_does_not_loop_forever` (`:791`),
  `the_tool_iteration_bound_stops_the_loop` (`:869`).
- **A success or a different failure resets the relevant counter.** `invalid`
  resets on any parseable batch (`crates/kerness/src/agent_runtime.rs:222`); `repeated` resets when a round is not
  all-error or differs from the previous round (`:264`). Tests:
  `a_recovering_model_is_not_penalised_for_an_earlier_bad_block` (`:854`),
  `a_failing_call_the_model_varies_is_left_alone` (`:815`),
  `a_tool_that_keeps_succeeding_is_never_cut_off` (`:835`).
- **Overflow leaves the continuation untouched.** `advance` propagates a
  context-overflow error without mutating the turn; [run.md](run.md) then calls
  `replace_history` (`crates/kerness/src/agent_runtime.rs:146`), which splices only the shared-history prefix and
  keeps the instruction and completed tool exchanges, before retrying
  (`crates/kerness/src/session/run.rs:758`, `:818`). No completed tool is
  replayed. A standalone legacy `run` cannot expose its private continuation, so
  it preserves its placeholder on a followup failure
  (`crates/kerness/src/agent_runtime.rs:410`); an opening overflow still
  propagates. Tests: `a_failed_followup_returns_a_placeholder` (`:976`),
  `a_failed_opening_call_returns_a_placeholder` (`:946`).
- **Strict versus absorbed provider errors.** Without `with_strict_errors`
  (`crates/kerness/src/agent_runtime.rs:368`) an ordinary provider failure becomes `TurnReason::ProviderFailure`
  with a placeholder text and the cause in `failure()` (`:130`); with it the
  error returns to the driver for a typed terminal outcome. Context overflow is
  never absorbed. Test: `:946`, `:976`, and
  `terminal_failures_cancellation_and_budgets_preserve_committed_work`
  (`crates/kerness/tests/session_run.rs:347`).
- **The agent's own effort rides on every call of the turn**, the followup
  after a tool result included (`chat`, `crates/kerness/src/agent_runtime.rs:445`, reads `self.agent.effort()`).
  Test: `the_agents_own_effort_rides_along_on_every_call_of_the_turn` (`:675`).
- **Native calls are authoritative; the fence parser is the fallback.**
  `calls_from` (`crates/kerness/src/agent_runtime.rs:473`) reads `response.tool_calls` under a native dialect and
  otherwise parses the content, so a model told about tools natively that still
  narrates one in prose is not stranded. Schemas are sent natively only when the
  dialect is not `Text` (`:445`). Tests:
  `a_native_call_round_trips_through_the_response_not_the_text` (`:1040`),
  `schemas_are_sent_natively_and_never_under_text` (`:1114`),
  `a_fenced_call_still_works_under_a_native_dialect` (`:1155`).
- **The result must answer the pending call by name.** `accept_tool_result`
  refuses a result for a different tool (`crates/kerness/src/agent_runtime.rs:249`). No dedicated test; the
  `validate` zip check at `:171` and the resume test
  `a_saved_native_approval_resumes_after_completed_tools_without_replaying_them`
  (`crates/kerness/tests/resume.rs:422`) exercise it indirectly.

### Extension points

- A new stop condition is a `TurnReason` variant plus one branch in
  `accept_response` (`crates/kerness/src/agent_runtime.rs:204`) or `accept_tool_result` (`:243`) and a matching
  arm in `validate`.
- A driver that wants different dispatch reuses `start`/`advance`/
  `accept_tool_result` and never calls `run`; that is what [run.md](run.md) does.

## Key Types and Entry Points

- `crates/kerness/src/agent_runtime.rs:51` — `AgentTurn` — private scratch,
  pending calls, completed results, loop guards, and record outbox; serialisable
  without carrying a provider or a handler. `new` at `:72`.
- `crates/kerness/src/agent_runtime.rs:154` — `snapshot()` / `:158`
  `from_snapshot(value)` — JSON in and out; restore validates the exact tool
  cursor and rejects inconsistent history or result positions with
  `Error::Session`.
- `crates/kerness/src/agent_runtime.rs:114` — `pending_call()` — the next
  unexecuted call, `None` once complete; inspect it before approval or
  journalling. `text()` at `:126`, `failure()` at `:130`, `reason()` at `:134`.
- `crates/kerness/src/agent_runtime.rs:243` — `accept_tool_result(result)` —
  commits one result, appends the dialect-rendered message, advances the
  cursor, and closes the round with guard bookkeeping; errors on no pending call
  or a name mismatch.
- `crates/kerness/src/agent_runtime.rs:140` — `take_recorded()` — drains the
  outbox once.
- `crates/kerness/src/agent_runtime.rs:146` — `replace_history(history)` —
  swaps the shared-history prefix after compaction; keeps everything the turn
  appended.
- `crates/kerness/src/agent_runtime.rs:293` — `AgentRunner<'a>` — borrows
  agent, provider, dispatcher, and a `messages_for` callback; `new` at `:312`,
  `with_max_tool_iterations` at `:333`, `with_record` at `:340`, `with_tools` at
  `:350`, `with_strict_errors` at `:368`.
- `crates/kerness/src/agent_runtime.rs:356` — `start(history, purpose,
  instruction)` — an `AgentTurn` with no IO; `:376` `advance(turn)` — one
  logical provider request, no tool executed, `Ok(None)` when already complete.
- `crates/kerness/src/agent_runtime.rs:410` — `run(history, purpose,
  instruction)` — the blocking driver: dispatches each pending call through the
  `ToolDispatcher` and advances until text; absorbs a followup provider error
  as a placeholder unless strict.
- `crates/kerness/src/agent_runtime.rs:39` — `TurnReason` — `Completed`,
  `ToolIterations`, `InvalidCalls`, `RepeatedFailures`, `ProviderFailure`.
  `MAX_INVALID_CALLS` at `:30`, `MAX_REPEATED_FAILURES` at `:34`,
  `FOLLOWUP_PROMPT` at `:26`.

## Interactions

- [run.md](run.md) owns the live `AgentTurn`, binds providers, prompt assembly,
  permissions, approval, persistence, cancellation, and usage budgets around
  each `advance`, and calls `accept_tool_result` after its own dispatch. Shared
  state: the turn value and its `snapshot()` under the checkpoint's
  `loop.runtime`. Tested in `crates/kerness/tests/session_run.rs`,
  `crates/kerness/tests/tools_e2e.rs`, and `crates/kerness/tests/resume.rs:422`.
- [loop.md](loop.md) requests whole turns without knowing how many provider or
  tool steps they require.
- [provider.md](provider.md) owns retries and wire transport;
  `chat_with_retries` is the one method this module calls, inside
  `usage::observe_provider_call` (`crates/kerness/src/usage.rs:603`).
- [toolkit.md](toolkit.md) validates and dispatches the single pending call;
  [toolschema.md](toolschema.md) renders the assistant turn and each result in
  the dialect's shape (`render_assistant_turn`, `render_tool_result`).
- [compaction.md](compaction.md) supplies the rewritten history that
  `replace_history` splices in.
- [sessionfile.md](sessionfile.md) persists the turn snapshot together with
  loop state.
- [bindings.md](bindings.md) exposes `AgentRunner.run` only; stepping from
  Python goes through `SessionRun.step`.

## How to Test

```sh
cargo test -p kerness --lib agent_runtime                                  # pass = 19 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_agent_runtime.py -q  # pass = 17 passed
cargo test -p kerness --test tools_e2e                                     # pass = 18 passed, 0 failed
```

- `crates/kerness/src/agent_runtime.rs:639` — `step_to_completion` — every
  tool-loop case below restores from its own snapshot after each step and
  asserts the typed reason, so a snapshot that lost a counter fails here.
- `crates/kerness/src/agent_runtime.rs:710` `tool_output_is_fed_back_and_the_final_text_returned`, `:734`
  `the_loop_runs_more_than_one_round`, `:753`
  `an_error_result_is_fed_back_rather_than_re_prompted`: the loop, and that an
  error result is information for the model rather than a re-prompt.
- `crates/kerness/src/agent_runtime.rs:774`, `:791`, `:869`: the three guards; `:815`, `:835`, `:854`: the three
  cases the guards must not fire on.
- `crates/kerness/src/agent_runtime.rs:914` `recording_captures_the_whole_exchange_when_asked`: the outbox is
  drained once and the drained state survives a snapshot.
- `crates/kerness/src/agent_runtime.rs:946` / `:976`: the placeholder on an opening and a followup failure; the
  followup test also proves an overflow retry preserves the instruction and
  completed tool output.
- `crates/kerness/src/agent_runtime.rs:1040`, `:1114`, `:1132`, `:1155`: two native exchanges, a restore between
  them, correlation IDs, and exactly two tool executions; schemas never sent
  under text; Anthropic results in a user message; a fenced call still read
  under a native dialect.
- `bindings/python/tests/test_agent_runtime.py:82` —
  `test_the_caller_history_is_not_mutated` — the boundary's own claim: the
  Python list a caller passed is untouched.
- `bindings/python/tests/test_agent_runtime.py:183`, `:194` — the legacy provider fixture overrides `chat_with_retries` and
  refuses direct `chat`, proving the logical boundary from Python.
- `crates/kerness/tests/tools_e2e.rs:144`, `:321`, `:347`: one turn in each
  dialect through a session; `:387`, `:400`, `:413`: unknown tool, schema
  violation, and failing handler each answered as text; `:427`, `:439`: the
  guards seen from a session.
- Gap: `accept_tool_result`'s name-mismatch refusal (`crates/kerness/src/agent_runtime.rs:249`) has no dedicated
  test.

## Review and Refactor Guide

- **Changing a guard or its threshold** → `accept_response` (`crates/kerness/src/agent_runtime.rs:204`),
  `accept_tool_result` (`:243`), `validate` (`:165`), the constants (`:30`,
  `:34`); the root's constants table and `crates/kerness/tests/public_api.rs`
  assert the values; tests `:774`, `:791`, `:869` and their Python twins
  (`bindings/python/tests/test_agent_runtime.py:129`, `:140`, `:160`).
- **Changing what a snapshot holds** → `AgentTurn` (`crates/kerness/src/agent_runtime.rs:51`) and `validate`
  (`:165`); [sessionfile.md](sessionfile.md) stores it under `loop.runtime`, so
  a field change is a schema-2 change and `crates/kerness/tests/resume.rs:422`
  must still restore. `deny_unknown_fields` means an added field breaks old
  files by design; document the migration in [run.md](run.md).
- **Changing dialect rendering** → this module only calls
  [toolschema.md](toolschema.md)'s `render_assistant_turn` and
  `render_tool_result`; change them there, then rerun `crates/kerness/src/agent_runtime.rs:1040`, `:1132`, and the
  three `tools_e2e.rs` dialect cases.
- **Changing error absorption** → `advance` (`crates/kerness/src/agent_runtime.rs:376`) and `run` (`:410`); the
  owned engine sets strict mode at `crates/kerness/src/session/run.rs:809`, so a
  change here alters `RunReason::Failed` behaviour there; tests
  `crates/kerness/src/agent_runtime.rs:946`, `:976`, and
  `crates/kerness/tests/session_run.rs:347`.
- **Safe extension points**: a `TurnReason` variant; a new `with_*` builder on
  `AgentRunner`; a new accessor on `AgentTurn`. Reuse `step_to_completion` for
  any new loop test.
- **Forbidden coupling**: no `crate::session` or `crate::orchestrator` import;
  no tool execution inside `advance`; no provider call inside
  `accept_tool_result`; the scratch buffer is never handed out mutably.
- **Compatibility checks**: `AgentRunner`'s Python keyword names
  (`bindings/python/src/runtime.rs:417`) and `run(history, purpose,
  instruction=None)` are public; `MAX_INVALID_CALLS`, `MAX_REPEATED_FAILURES`,
  and `FOLLOWUP_PROMPT` are re-exported constants.

### Improvement candidates (proposals, not accepted work)

- Add a unit test for the name-mismatch refusal in `accept_tool_result`;
  success: a case in the `agent_runtime` module that accepts a result named for
  the wrong tool and asserts `Error::Session`.
- Compare rendered results structurally for repeated-failure detection so a
  varying timestamp does not defeat the guard; success: `crates/kerness/src/agent_runtime.rs:791` still passes and
  a new case with a changing timestamp in the error text also stops.

## Open Gaps / Roadmap

- One logical provider request may block through provider-owned retries.
  Cancellation is cooperative between runtime steps.
- Tool handlers execute synchronously; an arbitrary handler needs its own
  deadline or cancellation support while it is running.
- Repeated-failure detection compares rendered results. A changing timestamp
  or other varying detail counts as a different failure.
