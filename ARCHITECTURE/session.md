---
eatmycode_version: "1.1.0"
---

# Session

## Goal

Assemble a validated harness from configuration, registered agents, tools,
skills, context and memory. This is the M1 preparation boundary and the public
entry point to M2–M3 execution: `Session::new` loads the gameplan and confines
the session's own write paths, the `add_*` methods register the roster and its
capabilities, `prepare` resolves everything against the harness contract, and
`start` or `run` hands the result to the owned engine.

It must not own live execution state, events, approvals or outcomes — those
belong to [run.md](run.md) — nor the contract it validates against, which
[harness.md](harness.md) owns. Python exposes the Rust behavior through
[bindings.md](bindings.md) and decides nothing of its own.

## Status

`done` — owned preparation, the blocking compatibility API, complete and
contextual tool registration and the resource lifecycle are implemented.
`cargo test -p kerness session` passes 68 tests (39 inline unit tests plus the
`session_run` integration file and the session-filtered cases elsewhere);
`cargo test -p kerness --test session_run --test tools_e2e --test public_api`
passes 52; `bindings/python/tests/test_session.py` passes 122 and
`bindings/python/tests/test_examples.py` passes 10.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session.rs` | `SessionConfig`, `SessionResult`, `Memories`, `Shared`, `Session`; registration, preparation, prompt assembly, context fitting, the session-file adapter and the public `LoopHost` compatibility impl |
| `crates/kerness/src/session/run.rs` | Owned execution; documented in [run.md](run.md) |
| `crates/kerness/src/session/capabilities.rs` | Contextual tool identity and capabilities; documented in [run.md](run.md) |
| `crates/kerness/src/session/outcome.rs` | Strict result validation; documented in [run.md](run.md) |
| `bindings/python/src/session.rs` | `PySession`, `PySessionResult`, `PySessionMemory`, `PyFilter`; keyword-argument construction and the consumed-session slot |
| `bindings/python/kerness/session.py` | Re-export shim: `Session`, `SessionResult`, `SessionRun`, `RunControl`, `ToolContext`, `Message` and three constants |

## Language and Conventions

One Rust crate module with three submodules, one PyO3 binding module, one
Python shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `crates/kerness/src/session.rs` is the crate's largest file and the only one
  that imports every other module; nothing below it imports `crate::session`
  (verified by the import graph in the root's System Design). Its submodules
  reach back through `super::{lock, store_for, Shared, ...}`.
- Mutex recovery is `lock` (`crates/kerness/src/session.rs:373`), which takes
  a poisoned guard's inner value rather than panicking, because a tool handler
  that unwound must not take the session down with it. Every `Mutex` in this
  module goes through it; observed, no lint enforces it.
- Five `#[allow(clippy::too_many_arguments)]` sites in
  `bindings/python/src/session.rs` (`:60`, `:282`, `:349`, `:483`, `:632`)
  cover the keyword-argument constructors; the Python signature is spelled in
  the `#[pyo3(signature = (...))]` attribute directly above each.
- `SessionResult` derives `Serialize`/`Deserialize` (`:80`) because it travels
  inside the run checkpoint; `SessionConfig` does not, because it carries
  `Arc<dyn Provider>` and friends and is never persisted.
- Unit tests live inline at `crates/kerness/src/session.rs:2251` with their
  own doubles — `SequenceProvider`, `CaptureChannel`, `RecordingStore` — and
  `crate::testing::TempDir` (`:2258`); `confined` (`:2377`) is the policy every
  file-writing test needs because a scratch directory is outside the process's
  current directory. Integration tests use the doubles in
  `crates/kerness/tests/common/mod.rs`; Python tests use the conftest
  `MockProvider` family and `Test<Behaviour>` classes.

## Design and Invariants

### Decomposition

`Session` (`crates/kerness/src/session.rs:509`) holds configuration that a
borrow can reach: the gameplan, limits, defaults, agents, skills, context
sources, the conversation and the dispatcher. `Shared` (`:338`) holds what a
`'static` callback needs — channel, provider, access manager, memories, the
registered tool list, the harness-permitted and per-agent tool lists, the
turn's skill activation, the skills registry and cache, the context cache and
the memory filter — behind one `Arc` every built-in tool handler closes over.
`Memories` (`:230`) is the installed store plus the session scope and each
agent's own scope; `Session::memories` (`:703`) hands a live handle out so a
caller reads what the run wrote rather than a snapshot.

### Registration

`add_agent` (`:737`) resolves the role only far enough to select a chair and
reject a second orchestrator; every other inherited option waits until
preparation so defaults set after registration still apply (`resolve_agents`,
`:1527`, documents why). `add_tool_spec` (`:796`) refuses the reserved `Skill`
name and duplicates; `add_tool` (`:785`) is its one-line convenience.
`add_contextual_tool` (`:817`) registers a placeholder `ToolSpec` whose plain
handler refuses to run and stores the real handler in `contextual_tools`, so a
contextual tool advertised to the model can only execute through the owned
run. `add_context` (`:848`) requires a unique non-empty name because the name
becomes the prompt subheading a gameplan narrows by.

### Preparation

`prepare` (`:953`) runs once, at `start` or `run`, and fails before any
provider call: it settles agent defaults, checks the tool-history dialect
(`check_tool_history_dialect`, `:1581`), validates the harness against the
registered tools and context sources, narrows per-agent tools
(`resolve_agent_tools`, `:1460`: an agent may narrow the permitted set and
never widen it), renders every permitted context source once per agent
(`resolve_context`, `:1492`), resolves personas and skills, refuses a skill
requiring a tool nobody registered (`check_required_tools`, `:1926`), builds
every participant prompt and the orchestrator prompt, opens every memory scope
through the store, computes the checkpoint identity, and restores or seeds the
conversation. Preparation freezes roster and tool configuration through
ownership: `start` (`:914`) consumes the session, and `run` (`:900`) drives a
private `run_copy` (`:920`) then copies observable state back.

### Tool composition order

`Shared::active_tools` (`:457`) is the one place the four subtractive levers
compose: the registered list narrowed by the harness's permitted set, then by
the turn agent's own list, then the `Skill` tool is added for the active
activation, then the activation's gate narrows and its requirements add back
out of the registered list. `start_activation` (`:479`) is called at the top of
every turn so a loaded skill body and its gate are bounded to that turn.

### Memory paths and the trust boundary

`remember` (`:271`) and `revise_memory` (`:300`) are the only two paths model
output takes into a store, and both apply the configured `MemoryFilter` first.
They are free functions because the tool handlers outlive any borrow of the
session. `store_for` (`:323`) takes the store and scope under one lock and
releases it before the store is called, so a store written in Python that
re-enters the session cannot deadlock. Every scope opens before the first turn
and preparation failure, completion, terminal failure, cancellation and
abandonment each close opened resources once (`store_open`, checked by
[run.md](run.md)'s `close`).

### Write-path confinement

`Session::new` (`:549`) checks the memory file the store names, the session
file and every channel destination against the access policy before returning,
so a misplaced path fails at construction rather than mid-run. A store keeping
nothing on disk answers `None` from `path` and is checked against nothing.
`AccessPolicy::new()` rather than `default()` is the fallback, because the two
differ on `trust_skill_bundles` ([access.md](access.md)).

### Context fitting

`prompt_overhead` (`:1706`), `context_ceiling` (`:1736`) and
`fit_conversation` (`:1752`) measure the assembled system message against the
smaller of the session's ceiling and the agent's provider window, and refuse a
prompt that alone exceeds it; the algorithm is [compaction.md](compaction.md)'s
and the owned run schedules it as its own step.

### Compatibility adapter

`impl LoopHost for Session` (`:1846`) remains for callers driving an
`OrchestratorLoop` directly: `turn` (`:1140`) retries one context overflow
through `fit_conversation` at `OVERFLOW_RETRY_FRACTION` (`:77`), `deliver`
strips memory markers on every routed turn, and `record_position` (`:1897`)
stores the scheduler snapshot that `save` (`:1680`) writes. Compatibility
`run()` can reopen legacy completed snapshots from their saved counters
(`prepare`, the `legacy` branch after `resume`) and retain pending routing on
interrupted runs; explicit `start()` keeps terminal checkpoints terminal.

## Key Types and Entry Points

- `crates/kerness/src/session.rs:111` — `SessionConfig` — gameplan, topic,
  session-wide provider and model, agent defaults, channel, memory scope and
  store, session file, context ceiling, access policy and loop limits;
  `Default` (`:192`) selects the `debate` gameplan and a one-second turn delay.
- `crates/kerness/src/session.rs:549` — `Session::new(config)` — load and
  validate the gameplan, build the access manager and shared state, and confine
  the session's own write paths. Fails with `Error::GameplanLoad` or
  `Error::AccessDenied` before any agent is registered.
- `crates/kerness/src/session.rs:737` — `add_agent(agent)` — resolve the role
  spec to a chair, refuse a second orchestrator or a role file that does not
  exist, and pin a found role path absolute.
- `crates/kerness/src/session.rs:796` — `add_tool_spec(spec)` — register a
  complete specification including `takes_actor`; reserved and duplicate names
  are refused with the same messages `add_tool` uses.
- `crates/kerness/src/session.rs:817` — `add_contextual_tool(tool)` — register
  a handler that receives the engine's invocation identity and scoped
  capabilities; advertised like any tool, executable only through a run.
- `crates/kerness/src/session.rs:900` — `Session::run()` — blocking adapter
  over the owned runtime with legacy result coercion, callback approvals and
  provider-error placeholders; leaves the session usable afterwards.
- `crates/kerness/src/session.rs:914` — `Session::start(options)` — consume
  the configuration and return an owned `SessionRun`.
- `crates/kerness/src/session.rs:953` — `prepare(mode, legacy)` — resolve the
  roster, defaults, permitted tools and context, personas, skills and prompts;
  open memory scopes; restore or seed the conversation; return the scheduler.
- `crates/kerness/src/session.rs:457` — `Shared::active_tools()` — the
  composed tool set the dispatcher and the prompt both read for the current
  turn.
- `crates/kerness/src/session.rs:81` — `SessionResult` — committed transcript,
  completed turns and rounds, phase and end reason, summary and declared
  fields; `summary()` (`:103`) is the Python-facing name.

## Interactions

- [gameplan.md](gameplan.md), [harness.md](harness.md), [agent.md](agent.md)
  and [role.md](role.md) supply the configuration contract and roster:
  `load_gameplan` at construction, `validate_harness` and `Permitted` in
  `prepare`, `Agent::inherit` in `resolve_agents`, `role_file` and `load_role`
  in `add_agent`. Integration: `crates/kerness/tests/harness_contract.rs`,
  `crates/kerness/tests/session_run.rs:1197`.
- [access.md](access.md), [skills.md](skills.md) and [toolkit.md](toolkit.md)
  define access enforcement and the available tools: the manager built at
  `crates/kerness/src/session.rs:563`, `SkillRegistry` with its grant callback at `:601`, `default_tools`
  (`:2017`) and `ToolDispatcher` over `active_tools`. Integration:
  `crates/kerness/tests/access_e2e.rs`, `skills_e2e.rs`, `tools_e2e.rs`.
- [prompting.md](prompting.md), [context.md](context.md),
  [memory.md](memory.md) and [conversation.md](conversation.md) assemble inputs
  and retain committed work: `Shared::prompts` (`crates/kerness/src/session.rs:489`) binds the assembler's
  callbacks to this session's caches; `remember` and `revise_memory` are the
  store's only model-facing entry.
- [run.md](run.md), [loop.md](loop.md) and
  [agent-runtime.md](agent-runtime.md) execute the prepared harness: `start`
  hands over a `Session` plus the `OrchestratorLoop` `prepare` built;
  `attempt_turn` (`crates/kerness/src/session.rs:1166`) is the compatibility path's `AgentRunner` driver.
- [compaction.md](compaction.md), [sessionfile.md](sessionfile.md) and
  [channel.md](channel.md) handle context fitting, checkpoints and output:
  `fit_conversation`, `resume` (`crates/kerness/src/session.rs:1650`) and `save` (`:1680`),
  `record_and_emit` (`:1623`).
- [bindings.md](bindings.md) constructs a `SessionConfig` from keywords
  (`bindings/python/src/session.rs:350`) and keeps the consumed slot
  (`:251`); a started session refuses further use through `prepared`
  (`:263`).

## How to Test

```sh
cargo test -p kerness session                                                     # pass = 68 passed across binaries, 0 failed
cargo test -p kerness --test session_run --test tools_e2e --test public_api       # pass = 24 + 18 + 10, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q               # pass = 122 passed
.venv/bin/python -m pytest bindings/python/tests/test_examples.py -q              # pass = 10 passed
```

Rebuild the extension (`cd bindings/python && ../../.venv/bin/maturin
develop`) before the Python commands after a Rust change.

- `crates/kerness/src/session.rs:3422` —
  `an_installed_store_is_opened_read_written_and_closed` — the lifecycle
  owner: every scope opened before the first turn, notes routed to the store,
  callback access outside the locks, and once-only cleanup on completion,
  failure, cancellation and abandonment. `:3651`
  `a_store_that_cannot_open_stops_the_run_before_the_first_turn` and `:3557`
  `the_filter_runs_before_an_installed_store_sees_a_note` are the two ends of
  the memory path.
- `crates/kerness/src/session.rs:2556` — `a_second_orchestrator_is_refused_by_name`;
  `:2575` `each_missing_piece_of_a_run_is_named_by_the_error`; `:2641`
  `add_tool_refuses_a_name_it_could_not_honour` — registration and preparation
  refusals, each naming what was wrong.
- `crates/kerness/src/session.rs:3735` —
  `a_gameplan_naming_an_unregistered_tool_fails_the_session` and `:3698`
  `the_tools_key_decides_what_the_prompt_advertises` — harness narrowing
  through `active_tools`.
- `crates/kerness/src/session.rs:3789` —
  `a_second_run_picks_up_where_the_first_stopped` and `:3834`
  `a_file_written_for_another_topic_refuses_to_resume` — the compatibility
  adapter's resume path.
- `crates/kerness/src/session.rs:3873` —
  `a_prompt_over_the_limit_before_any_turns_fails_loudly`; `:3898`
  `the_smaller_of_the_session_and_the_provider_window_is_the_ceiling`;
  `:3943` `a_request_the_provider_calls_too_long_is_compacted_and_sent_again`;
  `:4002` `a_second_refusal_reaches_the_caller` — context fitting and the
  single overflow retry.
- `crates/kerness/tests/session_run.rs:1277` —
  `a_session_default_fills_every_agent_that_named_nothing`; `:1327`
  `an_agent_on_a_second_provider_must_name_its_own_model`; `:1360`
  `a_model_named_nowhere_says_where_to_write_one` — inheritance at
  preparation.
- `crates/kerness/tests/tools_e2e.rs:738` —
  `an_agent_cannot_grant_itself_a_tool_the_session_withheld` — the
  narrow-never-widen rule of `resolve_agent_tools`.
- `crates/kerness/tests/public_api.rs:134` —
  `a_session_assembles_from_the_public_api_alone` — the public surface is
  sufficient for a dependent.
- `bindings/python/tests/test_session.py:133` — `TestAddAgent`; `:401`
  `TestOwnedRunBoundary` (a consumed session refuses `run()` and `memory`);
  `:915` `TestContextSources`; `:1093` `TestSessionDefaults`; `:1155`
  `TestSessionContainment`; `:2487` `TestAStoreTheCallerWrote`; `:2923`
  `TestResumingFromASessionFile`.
- `bindings/python/tests/test_examples.py:132` —
  `test_every_name_it_reaches_for_still_exists` — every example script's
  `Session` method and `SessionResult` attribute resolves on the installed
  package.
- Gap: `run_copy` (`crates/kerness/src/session.rs:920`) is exercised only through `Session::run`; no test
  asserts which fields it copies back, so a new `Session` field left out of
  it is caught only if a test reads that field after `run()`.

## Review and Refactor Guide

- Adding a `SessionConfig` field → add it to `Default`
  (`crates/kerness/src/session.rs:192`), consume it in
  `Session::new` (`:549`), copy it in `run_copy` (`:920`) if the run reads it,
  add it to `contract` in `crates/kerness/src/session/run.rs:1400` if a saved
  run must match it, and add the keyword to
  `bindings/python/src/session.rs:324`. Test owner: `test_session.py` and
  `session_run.rs`.
- Adding an `add_*` registration → follow `add_context`
  (`crates/kerness/src/session.rs:848`): validate the
  name, refuse duplicates with a message naming the method, push in
  registration order; resolve it in `prepare` before the first provider call;
  chain-return `&mut Self` so Python's `PyRefMut` chaining keeps working
  (`bindings/python/src/session.rs:484`).
- Changing tool composition → `Shared::active_tools`
  (`crates/kerness/src/session.rs:457`) is the only
  site; `resolve` in [toolkit.md](toolkit.md) and `apply_gate` /
  `admit_required` in [skills.md](skills.md) are the primitives. Tests:
  `crates/kerness/src/session.rs:3698`, `crates/kerness/tests/tools_e2e.rs:677`
  through `:738`, `bindings/python/tests/test_session.py:727` and `:815`.
- Changing the memory path → `remember` (`crates/kerness/src/session.rs:271`), `revise_memory` (`:300`)
  and `default_tools` (`:2017`) must stay the only routes; tests
  `crates/kerness/src/session.rs:3177`, `:3283`, `:3557` and
  `bindings/python/tests/test_session.py:2022`.
- Changing what `prepare` validates → keep every refusal before the first
  provider call and name the agent or key at fault; `crates/kerness/tests/session_run.rs:1102`
  through `:1360` and `crates/kerness/src/session.rs:2575` assert the messages.
- Safe extension points: a new session-level default goes in `AgentDefaults`
  and `Agent::inherit` ([agent.md](agent.md)), never in a second fallback; a
  new built-in tool is added in `default_tools` with its access check inside
  the handler.
- Forbidden coupling: no lower module imports `crate::session`; no execution
  state on `Session` (that is `SessionRun`); no configuration mutation
  reachable from a tool handler after `start`.
- Compatibility checks: `crates/kerness/tests/public_api.rs:43` pins
  `DEFAULT_MAX_CONTEXT_TOKENS` and `OVERFLOW_RETRY_FRACTION`;
  `bindings/python/src/session.rs:324` and `:469` spell the Python keyword
  signatures that `test_examples.py` checks every example against.

Improvement candidates (proposals, not accepted work):

- Split `crates/kerness/src/session.rs` (4,092 lines) so registration,
  preparation and the compatibility `LoopHost` impl are separate files under
  `session/`. Benefit: a reviewer reads the file that owns the change. Check:
  the public surface at `:49` and every test name above unchanged; `cargo
  test -p kerness session` still passes 68.
- Add a unit test that constructs a `Session`, sets every configurable field,
  calls `run_copy` through `run()` with a zero-turn provider, and asserts each
  field survived. Benefit: closes the `run_copy` gap. Check: a deliberately
  dropped field in `run_copy` fails that test.

## Open Gaps / Roadmap

- Configuration and prompt/resource assembly remain in `session.rs`; live
  execution has its own [run.md](run.md) owner. The public `LoopHost` adapter
  supports callers driving the lower-level scheduler directly.
- One access policy and skill registry are shared by a synchronous run;
  parallel execution requires a new ownership contract (M4).
- Custom provider, channel, memory and tool implementations are host-owned
  objects. The engine cannot serialize their implementation or undo their
  external effects; hosts version them for resume.
- `turn_delay` is a real `thread::sleep` inside `record_and_emit`
  (`crates/kerness/src/session.rs:1623`)
  on the compatibility path; tests set it to zero and the owned run does not
  sleep, so the two paths differ in wall-clock behavior.
