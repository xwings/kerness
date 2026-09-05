# Session

## Goal

Assemble a validated harness from configuration, registered agents, tools,
skills, context and memory. This is the M1 preparation boundary and the public
entry point to M2–M3 execution. Runtime state and control belong to
[run.md](run.md); Python exposes the Rust behavior through
[bindings.md](bindings.md).

## Status

`done` — owned preparation, blocking compatibility, complete/contextual tool
registration and resource lifecycle are implemented. Rust integration and Python
boundary checks pass.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session.rs` | Configuration, registration, shared resources, preparation and public `LoopHost` compatibility |
| `crates/kerness/src/session/run.rs` | Owned execution; documented in [run.md](run.md) |
| `bindings/python/src/session.rs` | Python session configuration and conversion boundary |
| `bindings/python/kerness/session.py` | Public re-exports |

## Key Types and Entry Points

- `crates/kerness/src/session.rs:111` — `SessionConfig`: gameplan, defaults,
  resources, context ceiling and legacy loop settings.
- `crates/kerness/src/session.rs:549` — `Session::new`: load/validate the
  gameplan and confine session-owned write paths before execution.
- `crates/kerness/src/session.rs:796` — `add_tool_spec`: preserve the complete
  specification, including `takes_actor`; reserved, built-in and duplicate names
  use the same rejection rules as `add_tool`.
- `crates/kerness/src/session.rs:817` — `add_contextual_tool`: register a
  handler using the engine's invocation identity and scoped capabilities.
- `crates/kerness/src/session.rs:900` — `Session::run`: blocking adapter over
  the owned runtime, retaining legacy result/error/approval behavior.
- `crates/kerness/src/session.rs:914` — `Session::start`: consume configuration
  and return an owned `SessionRun`.
- `crates/kerness/src/session.rs:953` — `prepare`: resolve the roster,
  inherited defaults, permitted tools/context, personas, skills and prompts;
  open memory scopes and restore or seed conversation state.
- `crates/kerness/src/session.rs:230` — `Memories`: the installed store and
  session/per-agent scopes; `Session::memories` supplies a live host handle.
- `crates/kerness/src/session.rs:457` — `Shared::active_tools`: combine harness
  permissions, agent narrowing and active skill requirements for dispatch.
- `crates/kerness/src/session.rs:81` — `SessionResult`: committed history,
  completed turns, phase/round/end state, summary and declared fields.

`add_agent` resolves the role far enough to select a chair and reject a second
orchestrator. Other inherited options wait until preparation, so defaults set
after registration still apply. Agent workspaces intersect the session workspace;
an agent's explicit tool list narrows the harness permissions. Context selection
belongs to the harness: every permitted source resolves once for each agent,
receiving that agent's name, and its returned text is cached.

Preparation freezes roster/tool configuration through ownership. `Shared` holds
resources that `'static` callbacks need: access, memory, channels, cached context
and skills. Host callbacks execute outside the framework resource locks;
contextual tool handlers receive no mutable parent session. Legacy dispatcher
activation remains per-session and synchronous. New tool identity is assigned
by the owned runtime, independently of model arguments.

Every memory scope opens before a provider turn. Preparation failure, successful
completion, terminal failure, cancellation and abandonment close opened resources
once. Stores are cloned out of the memories lock before `path`, reads, appends
and cleanup callbacks. Model-generated memory passes through the configured
filter. A failed note append retains the cleaned, paid answer in committed
history before returning the original error. Successful summarizing-store
maintenance is stepped and metered separately from cleanup.

`run()` holds an exclusive borrow while driving a private configuration copy,
then copies observable session state back. Public `start()` consumes the
session. Compatibility `run()` can reopen legacy completed snapshots from their
saved counters and retain pending routing on interrupted runs; explicit `start()`
keeps terminal checkpoints terminal. See [run.md](run.md) for recovery contracts.

## Interactions

- [gameplan.md](gameplan.md), [harness.md](harness.md), [agent.md](agent.md)
  and [role.md](role.md) supply the configuration contract and roster.
- [access.md](access.md), [skills.md](skills.md) and [toolkit.md](toolkit.md)
  define access enforcement and available tools.
- [prompting.md](prompting.md), [context.md](context.md),
  [memory.md](memory.md) and [conversation.md](conversation.md) assemble inputs
  and retain committed work.
- [run.md](run.md), [loop.md](loop.md) and
  [agent-runtime.md](agent-runtime.md) execute the prepared harness.
- [compaction.md](compaction.md), [sessionfile.md](sessionfile.md) and
  [channel.md](channel.md) handle context fitting, checkpoints and output.

## How to Test

```sh
cargo test -p kerness session
cargo test -p kerness --test session_run --test tools_e2e --test public_api
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q
.venv/bin/python -m pytest bindings/python/tests/test_examples.py -q
```

All commands exit 0. The lifecycle owner
`an_installed_store_is_opened_read_written_and_closed` covers scoped open/read/
write, callback access outside locks, and once-only cleanup on completion,
failure, cancellation and abandonment. Registration tests exercise both legacy
and complete specifications. Public integration tests prove run/step equivalence,
host completion, partial-history failure behavior and scoped/expired tool access.
Python tests prove forwarding, consumed-session behavior, callback lifetimes and
legacy compatibility; example checks resolve the published API.

## Open Gaps / Roadmap

- Configuration and prompt/resource assembly remain in `session.rs`; live
  execution has its own [run.md](run.md) owner. The public `LoopHost` adapter
  supports callers driving the lower-level scheduler directly.
- One access policy and skill registry are shared by a synchronous run;
  parallel execution requires a new ownership contract (M4).
- Custom provider, channel, memory and tool implementations are host-owned
  objects. The engine cannot serialize their implementation or undo their
  external effects; hosts version them for resume.
