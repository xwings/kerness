# Session

## Goal

The top-level object. A `Session` loads the gameplan, validates the harness
against what was registered, builds the access manager, the skill registry, the
tool registry, the memory store, the context cache, and the conversation, then
runs — either the round-robin participant loop or the orchestrator loop — and
returns a `SessionResult`. It is the module every other one is reached through.

It is also the largest module in the crate, and deliberately so: assembling all
of that in one place is what keeps every other module free of knowledge about
the rest.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/session.rs` | `SessionConfig`, `Session`, `Shared`, `Memories`, `SessionResult`, the `LoopHost` impl |
| `bindings/python/src/session.rs` | `PySession`, `PySessionResult`, `PyFilter`, the parked-channel re-raise |
| `bindings/python/kerness/session.py` | re-export shim (`Message`, `Session`, `SessionResult`) |

## Key Types and Entry Points

- `crates/kerness/src/session.rs:106` — `SessionConfig` — everything a session
  needs, as data; `Default` at `:187`. The session-level answers an agent
  inherits — `provider`, `model`, `reasoning_effort`, `persona`, `language` —
  sit here, ahead of any agent, because that is the order a caller writes them
  in.
- `crates/kerness/src/session.rs:148` — `SessionConfig::memory_filter` — the
  caller's filter over what agents write to memory; `None` stores notes as
  written. `memory_store` at `:136` is the other half: where memory is kept, or
  `None` for the bundled `FileMemory`. See [memory.md](memory.md).
- `crates/kerness/src/session.rs:526` — `Session` — the assembled run.
- `crates/kerness/src/session.rs:567` — `new(config)` — where the gameplan is
  loaded and the harness validated, so an impossible contract fails before any
  provider call. Also where the session's own write paths — whatever the memory
  store names for the session scope, the session file, a channel's log — are
  checked against the workspace, so a misplaced one fails at construction rather
  than mid-turn.
- `crates/kerness/src/session.rs:891` — `run()` — the whole harness; returns
  `SessionResult`.
- `crates/kerness/src/session.rs:1468` — `resolve_agents()` — the first thing
  `run()` does after the pre-flight: every agent's unset option is filled from
  `AgentDefaults`, and every agent workspace is intersected with the session's.
  One mechanism, one place, so nothing downstream can forget a fallback.
- `crates/kerness/src/session.rs:1401` — `resolve_agent_tools(permitted)` — every
  agent that declared its own tool list, checked against the harness-permitted
  set. An agent narrows, so a name outside *permitted* is refused here, named
  with the agent, before the first provider call.
- `crates/kerness/src/session.rs:1433` — `resolve_context(permitted)` — every
  permitted context source called once per agent and cached; see
  [context.md](context.md).
- `crates/kerness/src/session.rs:1876` — `check_required_tools(...)` — a skill
  that declares `requires-tools:` for a tool nobody registered is refused here,
  before the first provider call.
- `crates/kerness/src/session.rs:474` — `active_tools()` — the five steps that
  decide what a turn is offered, four of them subtractive and the last additive;
  the order is documented on the function and is what makes a skill's requirement
  outrank both a gameplan's list and an agent's own.
- `crates/kerness/src/session.rs:1796` — `impl LoopHost for Session` — how the
  orchestrator loop drives it.
- `crates/kerness/src/session.rs:1211` — `build_orchestrator_prompt(participants)`
  — the gameplan body, the roster, the phase block, and the rules. Which flow
  rules it carries depends on whether the harness declared phases: without them
  the orchestrator controls the flow and is told so, and with them the loop does,
  so the orchestrator is told to work the briefing's pending list instead and not
  to write a participant's turn for it. Granting both is a contradiction the
  model resolves by fabricating the turns it never called — see
  [loop.md](loop.md).
- `crates/kerness/src/session.rs:753` — `add_agent(agent)` / `:788` `add_skill` /
  `:801` `add_tool` / `:838` `add_context` — the registration chain; each returns
  `&mut Self` so registration composes. Agents are otherwise stored verbatim:
  nothing else is resolved until `run()`, so a session default written after the
  roster still fills it.
- `crates/kerness/src/session.rs:226` — `Memories` — the installed store plus the
  scope each agent addresses it by, shared as `Arc<Mutex<_>>` so a handle onto
  the session's memory stays live after the run.
- `crates/kerness/src/session.rs:71` — `SessionResult` — topic, turns, consensus,
  history, summary, parsed fields, rounds, phase reached, end reason.
- `crates/kerness/src/session.rs:859` — `run_command` / `:877` `read_file` / `:882`
  `list_dir` — the built-in tools, each going through the access manager. A
  command with no working directory of its own starts at the actor's workspace.
- `crates/kerness/src/session.rs:56` — `DEFAULT_MAX_CONTEXT_TOKENS` — the
  compaction ceiling, re-exported as `kerness.session.DEFAULT_MAX_CONTEXT_TOKENS`
  so a caller sizing a context against it names the framework's number rather
  than copying it.
- `crates/kerness/src/session.rs:67` — `OVERFLOW_RETRY_FRACTION` — how far the
  conversation is compacted when a provider refuses a request for being too
  long. See [compaction.md](compaction.md).

### `Shared`, and what a `'static` handler can reach

Tool handlers are `Arc<dyn ToolHandler>` and the dispatcher's tool source is an
`Arc<dyn Fn>`: both are `Send + Sync + 'static`, so neither can hold a reference
to the session that built them. `Shared` (`session.rs:343`) is what they close
over instead — the access manager, the memories, the channel, and the per-turn
state.

Two fields exist only because the dispatcher is shared by every agent and takes
no argument saying which one is speaking. `agent_tools` (`session.rs:359`) holds
the list of each agent that declared one, and `turn_agent` (`session.rs:363`)
records whose turn it is, written by `start_activation` (`session.rs:496`)
alongside the skill activation it already recorded there. `active_tools` reads
both, which is what makes an agent's narrowing bind the dispatcher as well as
the prompt: a tool the agent gave up is neither advertised nor callable if the
model asks for it anyway.

`context_cache` (`session.rs:374`) is filled by `resolve_context` rather than
read through on demand, so a source that walks a tree costs one call per agent
per run and a source that fails does so before the first provider call.

### One door, and the one thing it settles

`add_agent` is the only way an agent joins a session, and which chair it takes is
carried by the `role` value rather than by which method the caller reached for.
It resolves `agent.role` far enough to learn the position, writes `agent.position`,
pins a resolved `.md` role to an absolute path, and enforces the one-orchestrator
rule so the error names the offending call.

That is the one thing settled at add time. Every option in
[agent.md](agent.md)'s inheritance table waits for `run()`, because an agent
added before the caller finished configuring the session would otherwise freeze
defaults that were not written yet. Role is the exception because it has nothing
to wait for: a session-wide role would make every agent the orchestrator at once,
so there is no session-level default to inherit, and a typo is knowable the
moment it is written. See [role.md](role.md).

`Session::new` sets `trust_skill_bundles` explicitly because `AccessPolicy`'s
derived `Default` and `AccessPolicy::new()` disagree on it; the reason is
recorded at `access.rs:284`.

On the Python side, `PySession` keeps the bound channel so that an exception a
delivery raised can be re-raised out of `run()` instead of arriving as the
framework error it had to be reduced to — see [channel.md](channel.md). A
`memory_filter` given from Python is bound by `bind_memory_filter`
(`bindings/python/src/session.rs:206`), which refuses a non-callable at
construction rather than at the first note an agent writes, mid-run and with the
session's work already spent. A `memory_store` is bound by `bind_memory_store`
(`bindings/python/src/memory.rs:136`), which takes the crate's store directly
when it is one and wraps anything else — see [memory.md](memory.md).

## Interactions

Session is the assembly point, so it touches nearly everything:

- Loads [gameplan.md](gameplan.md) and validates through [harness.md](harness.md).
- Builds [access.md](access.md)'s manager and [skills.md](skills.md)'s registry.
- Owns [conversation.md](conversation.md), [memory.md](memory.md), and
  [context.md](context.md)'s cache.
- Registers tools with [toolkit.md](toolkit.md).
- Runs turns through [agent-runtime.md](agent-runtime.md), and is the `LoopHost`
  for [loop.md](loop.md).
- Compacts through [compaction.md](compaction.md), saves through
  [sessionfile.md](sessionfile.md), delivers through [channel.md](channel.md).

## How to Test

```sh
cargo test -p kerness session                                        # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q  # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_examples.py -q # pass = 0 failed
```

- The Rust tests drive a `SequenceProvider` (`session.rs:2204`) through a fixed
  reply sequence with a `CaptureChannel` (`:2282`) recording what was delivered.
- `crates/kerness/src/session.rs:3339` — `an_installed_store_is_opened_read_written_and_closed`,
  and the three beside it, cover the memory slot from the session's side:
  [memory.md](memory.md) lists them.
- `bindings/python/tests/test_session.py` is the suite's integration layer,
  covering access refusals mid-run, per-agent memory, resume across two runs,
  compaction of a long run, and the two `run()`-time resolutions: session
  defaults filling agents (`:982` `TestSessionDefaults`) and workspaces composing
  by intersection (`:1044` `TestSessionContainment`). `:132` `TestAddAgent` covers
  the four role specs against the chairs they select, including the one that must
  not: prose reading like `orchestrator` seats a participant.
- `bindings/python/tests/test_session.py:704` — `TestPerAgentTools` — a declared
  list is what the agent is offered, an empty list leaves it none, a tool it gave
  up is not callable either, and a name the gameplan withheld stops the run
  before any provider call.
- `crates/kerness/tests/tools_e2e.rs` — the same four properties from Rust,
  asserted against the tool list the provider actually received.
- `crates/kerness/tests/session_run.rs` —
  `a_role_seats_an_agent_by_declaration_and_never_by_prose` and
  `a_missing_role_file_is_refused_where_it_was_named` — the same two properties
  from Rust, the second asserting the roster stays empty after the refusal.
- `bindings/python/tests/test_examples.py:132` — `test_every_name_it_reaches_for_still_exists` —
  parses each example's AST and resolves every imported name. It does not run
  them; it proves the public surface an example depends on has not moved.

## Open Gaps / Roadmap

- `session.rs` is 3,941 lines. `run()` in particular carries both the
  participant loop and the orchestrator handoff; splitting it would need a home
  for the shared setup that is not another module knowing about sessions.
- One access manager and one skill registry per session, shared by every agent.
  The manager is keyed by actor for the workspace; the allowlists are
  session-wide, deliberately — see [access.md](access.md).
- `turn_agent` is per-session mutable state standing in for an argument the
  dispatcher's signature does not carry. It is correct because a session is
  single-threaded and the field is written once per turn, but it is a fact about
  the run held in a `Mutex` rather than passed, and the step machine on the root
  roadmap is where that would be reconsidered.
- Resume restores the conversation and the loop state but not tool side effects;
  a resumed session re-runs nothing it already did and assumes the world moved on.
