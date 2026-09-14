---
eatmycode_version: "2.0.0"
---

# Session runtime

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing session/agent execution, scheduling, prompts/context, channel
delivery, contextual handle lifetime, core exports/utilities, or Rust examples.

## Responsibility and Status

Implemented: one owned engine serves automatic and host-driven runs.
Owns configuration resolution, run/turn state, scheduling, prompt assembly,
delivery and typed outcomes. Verification is recorded below; live model behavior
is not established by scripted tests. Explicit participant batches overlap
bounded provider calls; tools and observers stay on the caller's thread.

## Code Map

Paths are under `crates/kerness/`.

| Path / symbol | Role |
| --- | --- |
| [src/session.rs](../../crates/kerness/src/session.rs), `Session`; [src/agent.rs](../../crates/kerness/src/agent.rs) | Load, register and validate; resolve defaults, shared resources and persistence identity. |
| [src/session/run.rs](../../crates/kerness/src/session/run.rs), `SessionRun::step`; `src/session/{capabilities,outcome}.rs` | Owned continuation, events, approvals, invocation leases and result validation. |
| [src/agent_runtime.rs](../../crates/kerness/src/agent_runtime.rs), `AgentTurn`; [src/orchestrator.rs](../../crates/kerness/src/orchestrator.rs), `OrchestratorLoop` | Private provider/tool scratch; scheduler actions, phases and stopping rules. |
| [src/prompting.rs](../../crates/kerness/src/prompting.rs), `PromptAssembler`; `src/context.rs`, `src/channel.rs` | Prompt ordering, named context and delivery sinks. |
| `src/{lib,error,utils,pyfmt,logging}.rs`; [tests/session_run.rs](../../crates/kerness/tests/session_run.rs), [tests/public_api.rs](../../crates/kerness/tests/public_api.rs); `examples/` | Shared core surface/errors/rendering/retry/logging; public behavior and runnable consumers. Test-helper implementation belongs to build checks. |

## Local Conventions

[Root conventions](../../ARCHITECTURE.md#code-conventions) apply. Canonical
composition is `Session`: trait objects/closures with narrow shared resources,
not a second engine in the bindings. Session callbacks clone handles before
calling host code rather than holding the resource mutex through the callback.
Preserve Python-compatible repr, JSON formatting and coercion through `pyfmt.rs`
on both surfaces; prompt/error/result rendering is observable behavior.
Checkpoint types use Serde tagged snake_case enums and reject unknown fields;
preserve compatibility when extending them. Default output goes through
`ConsoleWriter`/`Logger`; host sinks may replace delivery. No nested agent files
currently add local rules.

## Contracts and Invariants

- `Session::start` consumes configuration into a run; `run` uses the same engine
  with legacy result coercion and callback approval behavior. Provider failures
  propagate unchanged from sequential and batch turns: `run` returns the original
  error, including HTTP status, URL and body; owned execution retains it in
  `RunOutcome.error` with typed termination and partial diagnostics
  (`session.rs`, `session/run.rs`, `session/outcome.rs`; `tests/session_run.rs`).
- A sequential step selects at most one logical provider/tool/compaction/maintenance
  operation, while settling local effects. A batch step may join a bounded group
  of provider calls. Provider retries and nested host
  calls may perform more work inside that operation. Cancellation is cooperative;
  terminal `Continue` repeats the outcome, other terminal inputs fail.
- Provider and model inherit as a pair: supplying an agent provider requires an
  explicit model. Persona/language/effort inherit separately. Role has no session
  default; only a role declaration seats the orchestrator. Workspace narrowing
  is delegated to access policy (`agent.rs`, `Session::prepare`).
- Agent tool exchanges stay in private scratch. Invalid calls/repeated failures
  terminate a turn, not silently restart tools. The dispatcher and prompt must
  see the same available tools (`agent_runtime.rs`, `Session` shared tools).
- For batch scheduling, per-turn activation, ordered commits, cancellation or
  batch snapshots, read [Concurrent batches](../topics/concurrent-batches.md).
- `ToolContext` identity is engine-created. Cloned handles expire with the
  invocation lease; preflight is side-effect-free. Approval IDs and frozen call
  arguments survive checkpoints; unknown interrupted effects require host
  reconciliation, not automatic replay (`session/capabilities.rs`, `run.rs`).
- Host-driven mode needs an orchestrator only if the harness requires it.
  Host `Finish` validates supplied fields without an implicit result-generation
  call; configured memory maintenance can still invoke a provider.
- Memory enters prompts as quoted, non-authoritative data with delimiters and
  a staleness caveat. Context sources are resolved/validated before execution;
  delivery errors and callback failures remain observable (`prompting.rs`,
  `context.rs`, `channel.rs`, `tests/harness_contract.rs`).

## Dependencies and Boundaries

The runtime composes [harness/assets](harness-assets.md), [providers](providers.md),
[tools/access](tools-access.md), and [memory/persistence](memory-persistence.md).
Read a partner when changing its validation, state or call contract. The runtime
owns `session/capabilities.rs`; tools/access owns the policy it consults. Providers
own usage measurement, while the run drives budget termination. Foundation
utilities are shared by lower modules; the composition direction does not imply
an enforced acyclic import graph. Read [Python bindings](python-bindings.md) for
any public signature, exception, callback or event/schema change.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| Run input, approval, checkpoint, cancellation or result | `SessionRun::step`, capabilities/outcome; `tests/session_run.rs`, `tests/access_e2e.rs`, `tests/resume.rs` | Read tools/access for grants and memory/persistence for saved state; update binding schemas and this owner. |
| Inheritance, prompt or scheduling | `Session`, `AgentTurn`, `OrchestratorLoop`, `PromptAssembler`; inline tests and `tests/harness_contract.rs` | Read harness/assets when changing accepted keys; memory/persistence when changing quoted/history content. |
| Export, error, delivery, utility or example | `src/lib.rs`, owning utility, `tests/public_api.rs`, matching Python test | Read Python bindings for surface parity; [build checks](../topics/build-checks.md) for cross-language validation and example builds. |

## Verification

Use the [root Rust suite/style commands](../../ARCHITECTURE.md#verification),
which include inline runtime tests, `session_run` outcomes/limits, `public_api`
exports/constants and `harness_contract` scheduling. Run from root:

| Check | Command | Pass evidence |
| --- | --- | --- |
| Host stepping | `cargo run -p kerness --example host_control` | Offline host selects a turn and finishes with validated fields. |
| Durable approval | `cargo run -p kerness --example resume_approval` | Offline saved approval resumes, each effect runs once. |

For exposed behavior also run the Python suite via [binding verification](python-bindings.md#verification).
Baseline validation results are tracked in [build checks](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

Cancellation cannot preempt blocking providers/callbacks. Batch provider waves
join before a step returns; continuous agents, inboxes, streaming and MCP/workflow
adapters are outside this contract.
The lower-level public runtime helpers remain supported alongside owned runs;
no replacement or removal schedule is established by source.
