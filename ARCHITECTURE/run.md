# Owned run engine

## Goal

Implement M1–M3 in Rust: own execution independently of mutable configuration,
expose host control at safe boundaries, enforce scoped tools and approvals,
persist suspended state, and return typed outcomes with usage. The Python API
forwards to this engine; execution decisions remain here.

## Status

`done` — automatic and host-driven execution, external/callback approval,
cooperative cancellation, versioned continuation, strict result diagnostics and
usage/budget admission are implemented and pass the commands below.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session/run.rs` | Run options/input/events, scheduler integration, durable runtime state and finalization |
| `crates/kerness/src/session/capabilities.rs` | Trusted invocation identity, preflight, expiring scoped tool handles |
| `crates/kerness/src/session/outcome.rs` | Declared-result validation and diagnostics |
| `crates/kerness/src/usage.rs` | Operation ledger and budgets; detailed in [provider.md](provider.md) |
| `bindings/python/src/run.rs` | Thin Python run, control and tool-context bindings |

## Key Types and Entry Points

- `crates/kerness/src/session/run.rs:70` — `RunOptions`: scheduling mode,
  approval mode, budgets/pricing, observer, result validation and host binding
  version. Defaults select automatic mode, external approvals and strict results.
- `crates/kerness/src/session/run.rs:264` — `SessionRun`: owned prepared session,
  scheduler, active turn, approvals, action intent, usage and terminal state.
- `crates/kerness/src/session/run.rs:382` — `step`: apply host input, then
  advance engine-selected work, settle ready local effects, or return a wait or
  terminal outcome.
- `crates/kerness/src/session/run.rs:102` — `RunInput`: continue, select an
  agent, add user text, approve, reconcile or finish with supplied JSON.
- `crates/kerness/src/session/run.rs:45` — `RunControl`: independent atomic
  cancellation handle, usable while a step holds the run's exclusive borrow.
- `crates/kerness/src/session/run.rs:205` — `RunEvent`: monotonic event sequence
  and stable run/turn/call correlation; `EventSink` is an observer.
- `crates/kerness/src/session/run.rs:360` — `checkpoint`: serialize coherent
  scheduler and runtime continuation through the session-file writer.
- `crates/kerness/src/session/run.rs:152` — `RunOutcome`: typed reason,
  committed `SessionResult`, diagnostics, usage and original error.
- `crates/kerness/src/session/capabilities.rs:108` — `ToolContext`: trusted
  identity and scoped file/directory/memory/command access, valid only during its
  handler invocation; `ContextToolHandler` declares preflight separately.
- `crates/kerness/src/session/outcome.rs:31` — `ResultDiagnostics`: distinguish
  missing result/field, malformed JSON, wrong type and unexpected field.

A step returns `Progress`, `Waiting` (host input, approval or indeterminate tool
intent), or `Finished`. Automatic scheduling follows the gameplan; host-driven
mode permits a single participant without an orchestrator unless the harness
explicitly requires one. Agent selection and user input require a turn boundary.
Approval/reconciliation address an identified suspended action mid-turn.
`Finish` validates the host's JSON at a safe boundary without an implicit
agent or judge call to generate the result. Configured successful memory
maintenance may still call a provider. A host may request an agent turn before
supplying its result.

Each step dispatches at most one engine-selected logical provider operation,
individual tool invocation, compaction or maintenance scope. It may settle
several ready local scheduler effects. A provider's synchronous implementation
can include retries/backoff; supplied provider retry and fallback seams are
individually metered. A tool can synchronously invoke several provider APIs:
their supplied metering seams inherit the tool actor and budget,
but arbitrary external I/O or opaque host overrides cannot be inspected.

Approval precedes an effect. Preflight must be side-effect-free and freezes the
request, arguments, actor and action identity. A command approval grants the
exact command and resolved working directory once and cannot override hard path
or host denials. Stale or mismatched decisions are recoverable input errors.
An arbitrary callback cannot suspend its stack for approval; it must declare
its action before invocation. Cloned contexts expire together on return or
unwind and cannot retain resource access after the handler ends.

Events are ordered observations, delivered at most once to the sink. When a
session file is configured, state is checkpointed before delivery; a sink failure becomes a failed outcome without
replaying the completed provider/tool action. Channels retain their existing
message contract. `drain_events()` exposes buffered run events. Sinks must not
re-enter `step`; control decisions use inputs or the independent cancel handle.
The engine commits completed provider replies before cancellation, a subsequent
budget stop or delivery failure, so paid answers remain in outcome history.
Unfinished private turn scratch remains in its continuation checkpoint.

Live active state holds an `AgentTurn` directly. Progress steps do not serialize
and reparse it; checkpoint restore validates its counters and native tool
positions before execution.

The schema-2 runtime lives under the existing snapshot's `loop.runtime`; the
scheduler lives under `loop.scheduler`. Continuation includes agent scratch,
pending calls/results, approval/decision, action intent, IDs, loaded skills,
context cache, usage and incremental maintenance. Restore validates counters,
identities and the resolved configuration contract. Callbacks/providers are
re-registered; `binding_version` is the host's version for implementations the
engine cannot serialize. Valid v1 snapshots migrate only at turn boundaries.

With `session_file` configured, intent is persisted before a tool's side
effect, and completion afterward. Without it, execution state is in memory and
`checkpoint()` is a no-op. A restored intent without completion waits for a
matching `Reconcile` result or cancellation; it never automatically reruns the tool. Checkpoint failure can
leave an indeterminate external effect. There is no exactly-once claim for
arbitrary tools or memory stores. Explicit terminal checkpoints remain terminal;
repeated `Continue` returns the same outcome without delivering events again.

Strict results preserve actual `false` and `0`, retain supplied values and report
contract failures as `InvalidResult`. `LegacyCoercion` is an explicit result-only
option; `Session::run()` separately preserves legacy provider-placeholder and
resume behavior. Typed reasons distinguish completed, cancelled, budget exceeded,
invalid result and failed. A malformed/stale host input remains a recoverable
error; a real operation budget stop or sink failure becomes terminal.

Usage aggregates by actor/provider/operation and preserves unknown token counts
and prices. Provider/tool operation limits prevent the next admitted operation.
Hard token/cost limits are rejected because no per-operation upper bound is
available; `MeasuredThreshold` may overshoot by the admitted operation. Elapsed
limits and cancellation are cooperative: provider calls and user callbacks can
block until they return. POSIX command polling observes cancellation sooner.

Successful memory maintenance runs one scope per step and is metered. A refused
retry becomes durable `BudgetExceeded` even when the store keeps its notes and
absorbs a provider error. Cleanup closes resources once without consolidation;
observed provider calls are refused during cancellation/failure/drop cleanup.

## Interactions

- [session.md](session.md) prepares and transfers ownership to the run.
- [agent-runtime.md](agent-runtime.md) owns resumable agent turns;
  [loop.md](loop.md) owns scheduling and pending delivery actions.
- [access.md](access.md), [toolkit.md](toolkit.md) and
  [memory.md](memory.md) implement the capabilities exposed during invocation.
- [provider.md](provider.md) owns metering; [compaction.md](compaction.md)
  supplies separately stepped context fitting.
- [sessionfile.md](sessionfile.md) validates/publishes checkpoints;
  [channel.md](channel.md) and [bindings.md](bindings.md) expose output and APIs.

## How to Test

```sh
cargo test -p kerness --test session_run --test tools_e2e --test access_e2e --test resume --test compaction_e2e --test public_api
cargo run -p kerness --example host_control
cargo run -p kerness --example resume_approval
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q
.venv/bin/python bindings/python/examples/host_control.py
```

All commands exit 0. Integration owners cover run/step equivalence; agent
selection and host finish without a judge; exact approval before effects;
stale inputs and hard denials; expired handles; interrupted tool reconciliation;
malformed snapshots; prior-turn preservation through repeated restore; strict
false/zero results; paid answers retained after memory/sink failure, cancellation
or budgets; per-step compaction/maintenance accounting; and budget admission for
provider calls from tools. Examples run without credentials or network.

## Open Gaps / Roadmap

- M4 streaming, workflow/MCP adapters, session-store operations, richer content
  and subagent scheduling remain deferred.
- Cancellation cannot forcibly interrupt arbitrary synchronous host code.
- Metering cannot inspect hidden retries or external I/O in opaque overrides;
  hard token/cost reservation needs a future provider-bound contract.
- Checkpoint contents include prompts, transcript and tool arguments. Session
  files are private, but the host controls storage, trust and retention.
