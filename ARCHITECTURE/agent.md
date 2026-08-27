# Agent

## Goal

An agent is a name, a model, a provider, and a system prompt — the unit a
session takes turns between. This module owns the agent record and the assembly
of its system prompt from the pieces the session supplies: persona text, skills
index, memory block, tool instructions, and the reasoning-visibility flag.
Serves **M2**.

`Role` distinguishes an orchestrator from a participant. The distinction is not
cosmetic: an orchestrator gets a different prompt, a different loop, and the
authority to address participants by name.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent.rs` | `Role`, `Agent`, system prompt and message assembly |
| `bindings/python/src/types.rs` | `PyAgent`, the pyclass callers construct |
| `bindings/python/kerness/agent.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/agent.rs:21` — `Role` — `Participant` or `Orchestrator`;
  `parse` at `:37` rejects anything else rather than defaulting.
- `crates/kerness/src/agent.rs:57` — `Agent` — name, model, provider, persona,
  system prompt, role, and the per-agent memory path.
- `crates/kerness/src/agent.rs:94` — `build_system_prompt(...)` — the base prompt
  plus persona, in the order a reader of the transcript would expect them.
- `crates/kerness/src/agent.rs:114` — `decorate_system_prompt(...)` — layers the
  optional parts on: skills index, memory block, tool prompt, reasoning note.
  Split from `build_system_prompt` because the session decorates a prompt it did
  not build, for an agent whose persona was already resolved.
- `crates/kerness/src/agent.rs:174` — `build_messages(...)` — the system message
  followed by history; this is exactly what goes to the provider.
- `crates/kerness/src/agent.rs:189` — `is_orchestrator()` / `:193`
  `is_participant()` — the role test the loop branches on.

`Agent` implements `Debug` by hand (`agent.rs:203`) because the provider handle
is a trait object with no useful representation, and a derived `Debug` would
print nothing legible in a test failure.

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
- `:105` — `test_the_two_roles_are_exclusive_and_participant_is_the_default` —
  and `:116` `test_an_unknown_role_is_rejected`.

## Open Gaps / Roadmap

- An agent's model is a plain string handed to its provider; there is no
  validation that the provider knows the model. The error surfaces on the first
  call, from the provider.
- Per-agent memory paths are supported, but the session's shared memory is the
  common case; two agents pointed at one file both see each other's writes (see
  [memory.md](memory.md)).
