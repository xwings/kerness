# Agent

## Goal

An agent is a name, a model, a provider, and a system prompt — the unit a
session takes turns between. This module owns the agent record and the assembly
of its system prompt from the pieces the session supplies: persona text, skills
index, memory block, tool instructions, and the reasoning-visibility flag.

Almost every field is an `Option`, and the absence is load-bearing: `None` means
*the session answers this*, and `Some` — including `Some("")` — means the agent
did. Collapsing the two into an empty-string sentinel would make "this agent
deliberately sets an empty system prompt" inexpressible.

`role` and `position` are the open and closed halves of one pair: `role` is the
spec the caller wrote — a built-in name, a `.md` path, or prose — and `position`
is the chair it selected, written back by `Session::add_agent`. The distinction
between the two positions is not cosmetic: an orchestrator gets a different
prompt, a different loop, and the authority to address participants by name. See
[role.md](role.md).

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent.rs` | `Agent`, system prompt and message assembly |
| `bindings/python/src/types.rs` | `PyAgent`, the pyclass callers construct |
| `bindings/python/kerness/agent.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/agent.rs:22` — `Agent` — name, model, reasoning effort,
  provider, persona, role and position, system prompt, skills, tools, the
  per-agent memory scope, and the per-agent workspace.
- `crates/kerness/src/agent.rs:61` — `skills` / `:72` `tools` — the two
  tri-state lists: `None` takes what the session permits, a list selects out of
  it, and `[]` opts out. Both narrow — see below.
- `crates/kerness/src/agent.rs:103` — `with_model(model)` / `:123`
  `with_provider(provider, model)` — the two ways to answer for an agent; the
  second takes both because a model name and a backend are a pair.
- `crates/kerness/src/agent.rs:114` — `with_role(role)` — stores the spec
  verbatim. It does *not* set `position`; only `Session::add_agent` reads one
  into the other, so an agent added nowhere is a participant.
- `crates/kerness/src/agent.rs:130` — `model_name()` / `:136` `effort()` — what a
  caller reads: the resolved value, or the default, never an `Option`.
- `crates/kerness/src/agent.rs:274` — `inherit(defaults)` — fills every unset
  option from the session's, and refuses the two configurations that cannot be
  filled. Called by `Session::resolve_agents` at the top of `run()`.
- `crates/kerness/src/agent.rs:313` — `AgentDefaults` — the session's answers,
  carried as one value so there is exactly one inheritance mechanism.
- `crates/kerness/src/agent.rs:141` — `build_system_prompt(...)` — the base prompt
  plus persona, in the order a reader of the transcript would expect them.
- `crates/kerness/src/agent.rs:157` — `decorate_system_prompt(...)` — layers the
  optional parts on: context blocks, skills index, memory block, tool prompt,
  reasoning note. Split from `build_system_prompt` because the session decorates
  a prompt it did not build, for an agent whose persona was already resolved.
- `crates/kerness/src/agent.rs:218` — `build_messages(...)` — the system message
  followed by history; this is exactly what goes to the provider.
- `crates/kerness/src/agent.rs:240` — `resolve_role()` — the base prompt: the body
  of the role file, or the prose itself, or the built-in `participant` role when
  the agent named none.
- `crates/kerness/src/agent.rs:250` — `is_orchestrator()` / `:254`
  `is_participant()` — the position test the loop branches on, a field read.

### Provider and model inherit as a pair

`inherit` refuses an agent that sets its own `provider` and leaves `model`
unset, naming the agent. Every other option inherits independently, and this one
does not because a model name is only meaningful on the backend it was written
for: a session-level `"gpt-5"` silently inherited by an agent pointed at
Anthropic is not a fallback, it is a wrong answer that surfaces as an opaque
provider error on the first call. `reasoning_effort` does inherit
independently — it is a portable enum, and `Provider::effective_effort` already
handles a backend that refuses it ([provider.md](provider.md)).

An agent with no model and a session with none either is the second refusal, and
it names both places a model can be written.

`AgentDefaults` carries no `system_prompt`. That option already falls back
through `build_system_prompt`'s `default_prompt` argument, and a second
mechanism for the same fallback is exactly the dead configuration the project
rules out.

`reasoning_effort` sits beside `model` rather than on the provider, and for the
same reason the model does: two agents can share one backend and still be asked
to think at different depths. It is an `Option<ReasoningEffort>`
([provider.md](provider.md)) whose unset state the session fills, reads back
through `effort()` as `High` when nobody named one, and rides along on every
provider call the agent makes — the follow-up after a tool result included.

`workspace` is the one field `inherit` does not touch. It composes by
intersection rather than override, so it is settled against the access manager,
which can refuse it — see [access.md](access.md).

### Tools narrow, and what that buys

`tools` is the third field that does not simply override the session's. A name
the gameplan did not permit is refused when the session resolves its agents
(`session.rs:1460`), before the first provider call and named with the agent, so
an agent cannot hand itself a tool the harness withheld. The reasoning is
[access.md](access.md)'s: a per-agent list under override semantics turns a
config file into a way around the contract, and the same syntax would spell both
the legitimate narrowing and the escalation.

What narrowing buys is not only reach. Under
[`ToolDialect::Text`](toolschema.md) every permitted schema is written out in
the system prompt, so an agent that calls two tools out of thirty otherwise pays
for thirty on every turn of the session. The narrowing binds the dispatcher as
well as the prompt — a tool the agent gave up is neither advertised nor callable
if the model asks anyway — and it is outranked only by a skill's
`requires-tools:`, which is a skill that agent chose to load. See
`Shared::active_tools` in [session.md](session.md) for the full order.

`skills` is the mirror image and deliberately not symmetrical: skills union with
the session's, because a skill is capability a session offers, while tools are
capability a harness grants.

`Agent` implements `Debug` by hand (`agent.rs:325`) because the provider handle
is a trait object with no useful representation, and a derived `Debug` would
print nothing legible in a test failure. It prints the identity and the
inheritable options and omits the session wiring — skills, tools, memory,
workspace — which are resolved elsewhere and would bury the fields a test
failure is about.

## Interactions

- Held by [session.md](session.md), which owns the agent list and decides turn
  order.
- Its prompt parts come from [prompting.md](prompting.md),
  [persona.md](persona.md), [memory.md](memory.md), and [skills.md](skills.md).
- Its `provider` is a [provider.md](provider.md) trait object.
- Driven one turn at a time by [agent-runtime.md](agent-runtime.md).

## How to Test

```sh
cargo test -p kerness agent::                                     # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_agent.py -q # pass = 0 failed
```

- `bindings/python/tests/test_agent.py:21` — `test_every_decoration_is_appended_to_the_base` —
  and `:12` `test_an_undecorated_agent_gets_the_prompt_it_was_given`: decoration
  adds, never replaces.
- `:88` — `test_the_system_prompt_leads_and_history_follows_in_order` — what
  `build_messages` guarantees.
- `:105` — `test_an_agent_on_its_own_is_always_a_participant` — and `:136`
  `test_position_is_read_only`: the chair is something a session grants, not
  something a constructor claims.
- `:127` — `test_any_string_is_a_role_because_prose_is_one` — there is nothing to
  reject in a role, which is why `position` is the closed half.
- `:145` — `test_the_level_is_unset_until_named_and_round_trips_as_its_name` — and
  `:158` `test_an_unknown_level_is_rejected`: `reasoning_effort` crosses the
  boundary as a validated string, the way `position` does.
- `bindings/python/tests/test_session.py:1093` — `TestSessionDefaults`: a session
  model filling the agents that named none, an agent on a second provider
  refused for naming no model, and a model named nowhere naming both places to
  write one. The Rust counterparts are in
  `crates/kerness/tests/session_run.rs`.
- `bindings/python/tests/test_session.py:815` — `TestPerAgentTools` — and
  `crates/kerness/tests/tools_e2e.rs`: what `tools` narrows, and that it cannot
  widen. Tested through a session because narrowing is resolved there, not on
  the record.

## Open Gaps / Roadmap

- An agent's model is a plain string handed to its provider; there is no
  validation that the provider knows the model. What `inherit` refuses is a
  model *silently crossing a provider boundary*, which is the failure the
  framework can see; whether a given backend has heard of a given name is still
  answered by the first call.
- Per-agent memory scopes are supported, but the session's shared memory is the
  common case; two agents pointed at one scope both see each other's writes (see
  [memory.md](memory.md)).
