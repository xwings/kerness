---
eatmycode_version: "1.2.0"
---

# Owned run engine

## Goal

Own execution independently of mutable configuration, expose host control at
safe boundaries, enforce scoped tools and approvals, persist suspended state,
and return typed outcomes with usage. This is the runtime: `SessionRun`
is the one state machine both `Session::run` and `Session::start` drive, and
the Python API forwards to it without making an execution decision of its own.

It must not own configuration or preparation — [session.md](session.md) does —
nor the scheduling policy inside a turn, which belongs to
[loop.md](loop.md) and [agent-runtime.md](agent-runtime.md). It consumes those
and adds identity, approval, durability, budget admission and finalization.

## Status

`done` — automatic and host-driven execution, external and callback approval,
cooperative cancellation, versioned continuation, strict result diagnostics and
usage/budget admission are implemented. `cargo test -p kerness --test
session_run --test tools_e2e --test access_e2e --test resume --test
compaction_e2e --test public_api` passes 89 tests;
`bindings/python/tests/test_session.py` passes 122; the two Rust examples and
the Python host-control example run offline and exit 0. The three source files
carry no inline `#[cfg(test)]` module: every claim below is proved through the
integration suite and the Python boundary.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session/run.rs` | Run options, input, events and outcomes; the `step` state machine; durable runtime snapshot and restore; finalization and cleanup |
| `crates/kerness/src/session/capabilities.rs` | Engine-assigned invocation identity, side-effect-free preflight actions, and the expiring `ToolContext` capability handle |
| `crates/kerness/src/session/outcome.rs` | Strict declared-result validation and its diagnostics |
| `crates/kerness/src/usage.rs` | Operation ledger, pricing and budget admission; owned by [provider.md](provider.md) |
| `bindings/python/src/run.rs` | `SessionRun`, `RunControl` and `ToolContext` pyclasses; contextual handler, preflight and event-sink callback translation |
| `crates/kerness/examples/host_control.rs` | Offline host-driven run: select an agent, observe events, finish with a validated result |
| `crates/kerness/examples/resume_approval.rs` | Offline suspended approval saved, dropped and resumed without repeating a finished tool |
| `bindings/python/examples/host_control.py` | The Python counterpart of the host-driven example |

## Language and Conventions

Rust crate submodules under `crates/kerness/src/session/`, exposed through the
`pub use` block at `crates/kerness/src/session.rs:49`; one PyO3 binding module.
The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- Every continuation and checkpoint type is `#[serde(deny_unknown_fields)]`:
  eight sites in `crates/kerness/src/session/run.rs` (`:92`, `:101`, `:151`,
  `:204`, `:213`, `:225`, `:232`, `:240`), two in
  `crates/kerness/src/session/capabilities.rs` (`:19`, `:45`) and one in
  `crates/kerness/src/session/outcome.rs` (`:30`). A field added to
  a saved shape is a schema change, not a silent default. Enforced by serde at
  restore.
- Wire enums use `#[serde(tag = "kind", rename_all = "snake_case")]`
  (`RunInput`, `WaitReason`, `RunReason`, `RunEventKind`, `PreflightAction`,
  `ResultIssue`) and `StepOutcome` uses `tag = "status"`
  (`crates/kerness/src/session/run.rs:161`). Those tags are the Python
  dictionary contract, so renaming a variant is a public change.
- `#[allow(clippy::large_enum_variant)]` at
  `crates/kerness/src/session/run.rs:164` is the crate's only non-binding
  `#[allow]`; the comment above it records why the outcome is owned.
- The thirteen bare `.unwrap()` calls in the crate's non-test code are all in
  `crates/kerness/src/session/run.rs`, each on a state-machine invariant that
  the preceding branch established (`self.active` checked before
  `advance_turn`, `self.terminal` set before `finalize` reads it). Observed,
  not enforced by a lint.
- The Python boundary releases the GIL around `step`
  (`bindings/python/src/run.rs:209`) and contextual `run_command` (`:166`), and
  reacquires it inside callbacks. `PyToolContext` and `PyRunControl` are
  `frozen` pyclasses (`:105`, `:172`); `PySessionRun` is not, so a reentrant
  `step()` fails on Python's mutable borrow.
- Tests are integration tests using the doubles in
  `crates/kerness/tests/common/mod.rs` (`ScriptedProvider`, `TempDir`) and the
  Python `MockProvider` family in `bindings/python/tests/conftest.py`.

## Design and Invariants

### Decomposition and dependency direction

`SessionRun` (`crates/kerness/src/session/run.rs:264`) owns a prepared
`Session`, the `OrchestratorLoop` scheduler, the active `AgentTurn`, the pending
approval and action intent, the usage collector and the terminal outcome. It
imports `access`, `agent_runtime`, `exec`, `jsonschema`, `orchestrator`,
`sessionfile`, `tooling`, `toolkit` and `usage`; nothing below it imports
`session`. `capabilities.rs` reaches only `access`, `exec` and `tooling`;
`outcome.rs` reaches only `harness` and `tooling`. Configuration is frozen at
`Session::start` (`crates/kerness/src/session.rs:914`) because the run takes the
session by value; the blocking `Session::run` (`:900`) drives a private copy and
copies observable state back.

### What a step promises

A step returns `Progress`, `Waiting` (host input, approval or indeterminate tool
intent), or `Finished`. Automatic scheduling follows the gameplan; host-driven
mode permits a single participant without an orchestrator unless the harness
explicitly requires one. Agent selection and user input require a turn boundary
(`require_boundary`, `crates/kerness/src/session/run.rs:510`). Approval and
reconciliation address an identified suspended action mid-turn. `Finish`
validates the host's JSON at a safe boundary without an implicit agent or judge
call to generate the result. Configured successful memory maintenance may still
call a provider. A host may request an agent turn before supplying its result.

Each step dispatches at most one engine-selected logical provider operation,
individual tool invocation, compaction or maintenance scope. It may settle
several ready local scheduler effects (`settle_ready`, `:646`). A provider's
synchronous implementation can include retries and backoff; supplied provider
retry and fallback seams are individually metered. A tool can synchronously
invoke several provider APIs: their supplied metering seams inherit the tool
actor and budget, but arbitrary external I/O or opaque host overrides cannot be
inspected.

### Approval precedes an effect

Preflight must be side-effect-free and freezes the request, arguments, actor and
action identity (`ApprovalRequest`, `:93`; `tool_step`, `:837`). A command
approval grants the exact command and resolved working directory once
(`ToolContext::run_command`, `crates/kerness/src/session/capabilities.rs:209`,
matched at `:226` and consumed at `:236`) and cannot override hard path or
host denials,
which `preflight` (`crates/kerness/src/session/run.rs:966`) checks before any
request is exposed. Stale or mismatched decisions are recoverable input errors
(`apply_input`, `:438`). An arbitrary callback cannot suspend its stack for
approval; it must declare its action before invocation. Cloned contexts expire
together on return or unwind: `InvocationLease` (`crates/kerness/src/session/capabilities.rs:117`) clears
the shared `live` flag on drop and `check_live` (`:160`) refuses every later
call.

### Events and delivery

Events are ordered observations, delivered at most once to the sink. When a
session file is configured, state is checkpointed before delivery (`emit`,
`crates/kerness/src/session/run.rs:1227`); a sink failure becomes a failed
outcome without replaying the completed provider or tool action. Channels keep
the message contract in [channel.md](channel.md). `drain_events()` exposes
buffered run events.
Sinks must not re-enter `step`; control decisions use inputs or the independent
cancel handle (`RunControl`, `:45`). The engine commits completed provider
replies before cancellation, a subsequent budget stop or delivery failure, so
paid answers remain in outcome history. Unfinished private turn scratch remains
in its continuation checkpoint.

Live active state holds an `AgentTurn` directly (`ActiveTurn`, `:214`).
Progress steps do not serialize and reparse it; checkpoint restore validates its
counters and native tool positions before execution (`restore`, `:1276`).

### Continuation and recovery

The schema-2 runtime lives under the snapshot's `loop.runtime`; the
scheduler lives under `loop.scheduler` (`checkpoint`, `:360`). Continuation
includes agent scratch, pending calls and results, approval and decision, action
intent, IDs, loaded skills, context cache, usage and incremental maintenance
(`RuntimeSnapshot`, `:241`). Restore validates counters, identities and the
resolved configuration contract: `contract` (`:1400`) renders the gameplan,
agents, tools, access policy, prompts, options and `binding_version`, and a
saved contract that differs is refused. Callbacks and providers are
re-registered; `binding_version` is the host's version for implementations the
engine cannot serialize. Valid v1 snapshots migrate only at turn boundaries.

With `session_file` configured, intent is persisted before a tool's side effect
and completion afterward (`ActionIntent`, `:226`, set at `:905` and written by
`emit`'s checkpoint at `:1239` before the handler runs at `:932`). Without
it, execution state is in memory and `checkpoint()` is a no-op. A restored
intent without completion waits for a matching `Reconcile` result or
cancellation; it never automatically reruns the tool (`advance`, `:523`).
Checkpoint failure can leave an indeterminate external effect. There is no
exactly-once claim for arbitrary tools or memory stores. Explicit terminal
checkpoints remain terminal; repeated `Continue` returns the same outcome
without delivering events again (`step`, `:382`).

### Outcomes and budgets

Strict results preserve actual `false` and `0`, retain supplied values and
report contract failures as `InvalidResult` (`outcome::validate`,
`crates/kerness/src/session/outcome.rs:36`). `LegacyCoercion` is an explicit
result-only option; `Session::run()` separately preserves legacy
provider-placeholder and resume behavior through `RunOptions::legacy`
(`crates/kerness/src/session/run.rs:82`). Typed reasons distinguish completed,
cancelled, budget exceeded, invalid result and failed. A malformed or stale host
input remains a recoverable error; a real operation budget stop or sink failure
becomes terminal.

Usage aggregates by actor, provider and operation and preserves unknown token
counts and prices. Provider and tool operation limits prevent the next admitted
operation (`usage.check_next()` before each dispatch; `begin_tool()` at
`:904`). Hard token/cost limits are rejected because no per-operation upper
bound is available; `MeasuredThreshold` may overshoot by the admitted operation.
Elapsed limits and cancellation are cooperative: provider calls and user
callbacks can block until they return. POSIX command polling observes
cancellation sooner.

Successful memory maintenance runs one scope per step and is metered
(`ClosingState`, `:233`; `advance_closing`, `:1164`). A refused retry becomes
durable `BudgetExceeded` even when the store keeps its notes and absorbs a
provider error. Cleanup closes resources once without consolidation (`close`,
`:1216`, guarded by `store_open`); observed provider calls are refused during
cancellation, failure or drop cleanup through
`usage::without_provider_calls`. `Drop` (`:1384`) calls the same `close` and
logs rather than panics.

## Key Types and Entry Points

- `crates/kerness/src/session/run.rs:70` — `RunOptions` — scheduling mode,
  approval mode, budget and pricing, event sink, result validation and host
  `binding_version`. Defaults select automatic mode, external approvals and
  strict results.
- `crates/kerness/src/session/run.rs:264` — `SessionRun` — the owned prepared
  session, scheduler, active turn, approvals, action intent, usage and terminal
  state. Constructed only by `Session::start` and `Session::run`.
- `crates/kerness/src/session/run.rs:382` — `SessionRun::step(input)` — apply
  host input, then advance engine-selected work, settle ready local effects, or
  return a wait or terminal outcome. Returns `Err` only for recoverable input
  mistakes; engine failures become a `Finished` outcome carrying the error.
- `crates/kerness/src/session/run.rs:102` — `RunInput` — `Continue`,
  `SelectAgent`, `UserMessage`, `Approve`, `Reconcile` and `Finish`; the
  Python `kind` dictionary contract.
- `crates/kerness/src/session/run.rs:45` — `RunControl` — independent atomic
  cancellation handle, usable while a step holds the run's exclusive borrow.
- `crates/kerness/src/session/run.rs:205` — `RunEvent` — monotonic `sequence`
  with stable run, turn and call correlation; `EventSink` (`:57`) is the
  observer trait, blanket-implemented for closures.
- `crates/kerness/src/session/run.rs:360` — `SessionRun::checkpoint()` —
  serialize the coherent scheduler and runtime continuation through the
  session-file writer; a no-op without a configured `session_file`.
- `crates/kerness/src/session/run.rs:152` — `RunOutcome` — typed `RunReason`,
  committed `SessionResult`, `ResultDiagnostics`, `UsageLedger` and the
  original `Error`, if any.
- `crates/kerness/src/session/capabilities.rs:108` — `ToolContext` — trusted
  identity and scoped file, directory, memory and command access, valid only
  during its handler invocation; `ContextToolHandler` (`:57`) declares
  `preflight` separately from `call`.
- `crates/kerness/src/session/outcome.rs:31` — `ResultDiagnostics` — `valid`
  plus a `ResultIssue` list distinguishing a missing result or field, malformed
  JSON, a wrong type and an unexpected field.

## Interactions

- [session.md](session.md) prepares and transfers ownership through
  `Session::start`; `Session::run` drives `run_to_completion`
  (`crates/kerness/src/session/run.rs:424`) with `RunOptions::legacy`.
  Integration: `crates/kerness/tests/session_run.rs`.
- [agent-runtime.md](agent-runtime.md) owns the resumable `AgentTurn` this run
  stores in `ActiveTurn`; [loop.md](loop.md) owns `OrchestratorLoop`, whose
  `LoopAction` values `advance` and `apply_effect` consume.
- [access.md](access.md), [toolkit.md](toolkit.md) and [memory.md](memory.md)
  implement the capabilities `ToolContext` exposes: `exec::read_file`,
  `exec::list_dir`, `exec::run_command_cancellable`, `store_for` and
  `Shared::remember`. Integration: `crates/kerness/tests/access_e2e.rs:588` and
  `:752`, `crates/kerness/tests/tools_e2e.rs:180`.
- [provider.md](provider.md) owns `UsageCollector`, `RunBudget`,
  `TokenPricing` and `UsageLedger` (`crates/kerness/src/usage.rs:323`, `:259`,
  `:121`, `:218`); the run installs the actor scope around every provider,
  tool, compaction and maintenance boundary.
- [compaction.md](compaction.md) supplies `fit_conversation`, which
  `advance_turn` (`crates/kerness/src/session/run.rs:724`) schedules as its own
  step under the `compaction` purpose.
- [sessionfile.md](sessionfile.md) validates and atomically publishes what
  `checkpoint` writes; the `loop.runtime` object is this module's schema and
  the envelope is that module's.
- [channel.md](channel.md) receives committed turns through `deliver` (`:1051`)
  and command logs; [bindings.md](bindings.md) exposes the run as
  `SessionRun`, `RunControl` and `ToolContext` with JSON-serialized inputs and
  outcomes.

## How to Test

```sh
cargo test -p kerness --test session_run --test tools_e2e --test access_e2e --test resume --test compaction_e2e --test public_api  # pass = 24 + 18 + 17 + 12 + 8 + 10 tests, 0 failed
cargo run -p kerness --example host_control     # pass = exit 0, validated host result, one provider call
cargo run -p kerness --example resume_approval  # pass = exit 0, restored approval, each tool once
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q   # pass = 122 passed
.venv/bin/python bindings/python/examples/host_control.py            # pass = exit 0
```

Rebuild the extension (`cd bindings/python && ../../.venv/bin/maturin
develop`) before the Python commands after a Rust change.

- `crates/kerness/tests/session_run.rs:257` —
  `a_host_selects_an_agent_and_finishes_without_a_judge_call` — host-driven
  agent selection and a host-supplied result validated with no closing
  provider call.
- `crates/kerness/tests/session_run.rs:347` —
  `terminal_failures_cancellation_and_budgets_preserve_committed_work` — paid
  answers stay in the outcome history after memory or sink failure,
  cancellation and budget stops; strict `false` and `0` results survive.
- `crates/kerness/tests/session_run.rs:643` —
  `memory_maintenance_is_stepped_metered_and_skipped_during_cleanup` —
  per-step maintenance accounting across complete, budget, cancel and drop.
- `crates/kerness/tests/tools_e2e.rs:180` —
  `contextual_tools_keep_actor_scope_and_expire_after_the_invocation` —
  scoped access, expired handles, and provider calls from inside a tool that
  cannot bypass the operation limit.
- `crates/kerness/tests/access_e2e.rs:588` —
  `external_approval_freezes_a_native_call_and_only_its_decision_can_execute_it`
  — exact approval before an effect, stale decisions refused. `:752`
  `command_preflight_checks_hard_denials_and_grants_only_the_frozen_command_once`
  — hard denials precede any request; a grant is single-use.
- `crates/kerness/tests/resume.rs:422` —
  `a_saved_native_approval_resumes_after_completed_tools_without_replaying_them`;
  `:625`
  `an_intent_without_completion_requires_reconciliation_and_never_replays_the_handler`;
  `:771` `a_failed_intent_write_prevents_the_tool_side_effect` — the
  continuation contract, malformed snapshots and forged identities refused.
- `crates/kerness/tests/compaction_e2e.rs` — compaction as a separately
  admitted step (8 tests).
- `bindings/python/tests/test_session.py:401` — `TestOwnedRunBoundary` —
  inputs, outcomes, events and handles cross as JSON values; a consumed
  `Session` refuses further use; cancellation from the control handle.
- Gap: no test drives a Python `event_sink` that raises, so the
  sink-failure-becomes-terminal path is proved from Rust only
  (`crates/kerness/tests/session_run.rs:347`).

## Review and Refactor Guide

- Changing an input, outcome or event shape → inspect `RunInput`,
  `StepOutcome`, `RunEventKind` and `WaitReason`
  (`crates/kerness/src/session/run.rs:102`, `:165`, `:173`, `:128`); the
  serde tags are the Python dictionary contract read by
  `bindings/python/src/run.rs:199` and documented in
  [bindings.md](bindings.md#owned-execution-and-contextual-tools). Update
  `TestOwnedRunBoundary` and the two Rust examples in the same change.
- Changing what a checkpoint holds → inspect `RuntimeSnapshot`
  (`crates/kerness/src/session/run.rs:241`),
  `snapshot` (`:1249`) and `restore` (`:1276`); every new field needs a restore
  validation rule and a case in `crates/kerness/tests/resume.rs`. A change to
  what `contract` (`crates/kerness/src/session/run.rs:1400`) renders
  invalidates every saved file, so treat it
  as a schema bump in [sessionfile.md](sessionfile.md).
- Changing approval or capability rules → inspect `tool_step`
  (`crates/kerness/src/session/run.rs:837`),
  `preflight` (`:966`), `ToolContext::run_command`
  (`crates/kerness/src/session/capabilities.rs:209`) and `check_live` (`:160`);
  tests in `crates/kerness/tests/access_e2e.rs:588`, `:752` and
  `crates/kerness/tests/tools_e2e.rs:180`.
- Changing budget admission → inspect the `usage.check_next()` and
  `begin_tool()` call sites in `advance`, `begin_turn`, `advance_turn` and
  `tool_step`, and `crates/kerness/src/usage.rs:259`; the test owner is
  `crates/kerness/tests/session_run.rs:347`.
- Safe extension points: a new `RunInput` variant handled in `apply_input`
  (`crates/kerness/src/session/run.rs:438`) with its own boundary rule; a new `PreflightAction` variant handled
  in `preflight` and `ToolContext::new`
  (`crates/kerness/src/session/capabilities.rs:125`); a new `RunEventKind`
  emitted through `emit` so it is checkpointed before delivery.
- Patterns to reuse: `usage.with_scope(actor, purpose, || ...)` around any new
  provider-bearing work; `InvocationLease` for any capability handed to a
  callback; `tool_error` (`crates/kerness/src/session/run.rs:1392`) for a refusal the model should read.
- Forbidden coupling: no execution or policy decision in
  `bindings/python/src/run.rs`; no reading of model arguments to build a
  `ToolIdentity`; no `Session` method that mutates configuration reachable
  from a `ToolContext`.
- Compatibility checks: `crates/kerness/tests/public_api.rs:91`
  (`the_root_re_exports_all_resolve`) pins the re-exported names at
  `crates/kerness/src/session.rs:49`; `bindings/python/src/session.rs:633`
  (`start`) spells the Python keyword signature.

Improvement candidates (proposals, not accepted work):

- Replace the thirteen invariant `.unwrap()` calls with `expect` messages naming
  the branch that established the invariant. Benefit: a violated invariant
  names itself. Check: `grep -c '\.unwrap()' crates/kerness/src/session/run.rs`
  returns 0 and the integration suite is unchanged.
- Drive a raising Python `event_sink` from `test_session.py`. Benefit: the
  Python side of the sink-failure contract is proved. Check: one new case in
  `TestOwnedRunBoundary` asserting `status == "finished"` with reason `failed`.

## Open Gaps / Roadmap

- M4 streaming, workflow and MCP adapters, session-store operations, richer
  content and subagent scheduling remain deferred; see the root
  [Roadmap](../ARCHITECTURE.md#roadmap).
- Cancellation cannot forcibly interrupt arbitrary synchronous host code.
- Metering cannot inspect hidden retries or external I/O in opaque overrides;
  hard token/cost reservation needs a future provider-bound contract.
- Checkpoint contents include prompts, transcript and tool arguments. Session
  files are private, but the host controls storage, trust and retention.
- The three files under `crates/kerness/src/session/` have no inline unit
  tests; every behavior is proved through whole-session integration tests,
  which is a longer path to a failing assertion than a unit test would be.
