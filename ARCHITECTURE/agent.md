---
eatmycode_version: "1.2.0"
---

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

This module does not own inheritance *timing*, workspace confinement, or tool
narrowing: it supplies `inherit` and the tri-state lists, and
[session.md](session.md) decides when to apply them and against what.

## Status

`done`. `cargo test -p kerness agent::` passes 14 tests and
`.venv/bin/python -m pytest bindings/python/tests/test_agent.py -q` passes 14;
the session-level consequences — defaults filling, provider/model pairing, role
seating, tool narrowing — pass in `crates/kerness/tests/session_run.rs` and
`crates/kerness/tests/tools_e2e.rs`.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/agent.rs` | `Agent`, `AgentDefaults`, system prompt and message assembly, `inherit` |
| `bindings/python/src/types.rs:645` | `PyAgent`, the pyclass callers construct; getters for every field and setters for all but `position` |
| `bindings/python/kerness/agent.py` | re-export shim |

## Language and Conventions

One Rust crate module and one pyclass inside the binding's `types.rs`; the
Python file is a three-line shim. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- **Builder style, owned `self`.** `with_model`, `with_role`, `with_provider`
  (`crates/kerness/src/agent.rs:103`, `:114`, `:123`) take and return `Self`;
  every field is also `pub`, so a literal works too. Observed, not enforced.
- **Hand-written `Debug`** (`crates/kerness/src/agent.rs:325`) because the
  provider is a trait object with no useful representation; it prints identity
  and the inheritable options and omits skills, tools, memory, and workspace.
- **`Cow` in prompt decoration** (`crates/kerness/src/agent.rs:157`): a prompt
  is copied once, at the end (`:194`), rather than once per placeholder or
  decoration; each addition promotes to `Owned`.
- **The pyclass constructor carries `#[allow(clippy::too_many_arguments)]`**
  (`bindings/python/src/types.rs:685`) — twelve keyword parameters in
  `#[pyo3(signature = ...)]` order, every one optional but `name`. `position`
  is a getter with no setter (`:791`); `reasoning_effort` crosses as a
  validated string through `ReasoningEffort::parse`.
- **Tests.** Unit tests use `alice()` (`crates/kerness/src/agent.rs:346`),
  `crate::testing::TempDir` (`:344`) for persona files, and a `StubProvider`
  whose `chat` is `unreachable!` because inheritance never calls a backend.
  The Python suite uses `tempfile` for persona files and sentence-style test
  names under `Test<Behaviour>` classes.

## Design and Invariants

### Decomposition and dependency direction

`agent` depends on `error`, `persona`, `provider`, `pyfmt`, and `role`, and on
nothing above them; it does not know sessions exist. Above it,
[session.md](session.md) calls `add_agent` (`crates/kerness/src/session.rs:737`)
to seat the chair, `resolve_agents` (`:1527`) to inherit and confine, and
`resolve_agent_tools` (`:1460`) to narrow; [agent-runtime.md](agent-runtime.md)
reads `effort()` on every provider call
(`crates/kerness/src/agent_runtime.rs:461`).

Two prompt paths share one decorator. `build_system_prompt` (`crates/kerness/src/agent.rs:141`) picks the
base — the agent's own prompt or the session's default — and
`decorate_system_prompt` (`:157`) layers persona, reasoning note, language,
skills index, and placeholder substitution on top. The split exists because the
session decorates an orchestrator prompt it built itself; keeping the decoration
in one place is what stops participant and orchestrator prompts drifting apart.

### Invariants a change must preserve

- **`None` means the session answers; `Some("")` is an answer.** No field
  collapses absence into an empty string. Test:
  `a_session_that_pins_nothing_leaves_the_agent_pinning_nothing`
  (`crates/kerness/src/agent.rs:555`) and
  `test_an_unnamed_role_is_none_rather_than_a_default_string`
  (`bindings/python/tests/test_agent.py:119`).
- **Provider and model inherit as a pair.** `inherit` (`crates/kerness/src/agent.rs:274`) refuses an agent
  that sets `provider` and leaves `model` unset, naming the agent, and refuses
  a model named nowhere, naming both places to write one. A model name is only
  meaningful on the backend it was written for; a session-level `"gpt-5"`
  silently inherited by an agent pointed at Anthropic is a wrong answer that
  surfaces as an opaque provider error. `reasoning_effort` does inherit
  independently — it is a portable enum, and `Provider::effective_effort`
  handles a backend that refuses it ([provider.md](provider.md)). Tests:
  `an_agent_with_its_own_provider_must_bring_its_own_model` (`:573`),
  `a_model_named_nowhere_is_an_error_that_says_both_places_to_set_it` (`:597`),
  and through a session `crates/kerness/tests/session_run.rs:1327`, `:1360`.
- **Inheritance runs once, at run start, never at `add_agent`.**
  `resolve_agents` (`crates/kerness/src/session.rs:1527`) calls `inherit` so
  defaults set after registration still apply; `add_agent` settles only the
  position, which has no session-level default to wait for. Test:
  `a_session_default_fills_every_agent_that_named_nothing`
  (`crates/kerness/tests/session_run.rs:1277`).
- **`AgentDefaults` carries no `system_prompt`.** That option already falls
  back through `build_system_prompt`'s `default_prompt` argument; a second
  mechanism for the same fallback is the dead configuration the project rules
  out (`crates/kerness/src/agent.rs:313`). No test enforces the absence; the
  struct definition is the evidence.
- **Position comes from a declaration, never from prose or a constructor.**
  `with_role` (`crates/kerness/src/agent.rs:114`) stores the spec verbatim and does not set `position`;
  only `Session::add_agent` reads a role *file*'s frontmatter into it, so an
  agent constructed and added nowhere is a participant. The Python getter has
  no setter (`bindings/python/src/types.rs:791`). Tests:
  `a_named_role_is_kept_verbatim_until_the_session_reads_it`
  (`crates/kerness/src/agent.rs:609`),
  `the_two_positions_are_exclusive_and_participant_is_the_default` (`:481`),
  `test_position_is_read_only` (`bindings/python/tests/test_agent.py:136`),
  `a_role_seats_an_agent_by_declaration_and_never_by_prose`
  (`crates/kerness/tests/session_run.rs:1197`).
- **A missing persona file fails; it is never pasted as text.** `resolve_persona`
  (`crates/kerness/src/agent.rs:208`) loads any `.md` and propagates `NotFound`, because
  `Persona: ./personas/typo.md` in a system prompt lets a run complete looking
  healthy while costing real provider calls. Test:
  `a_missing_persona_file_fails_and_names_what_it_tried` (`:436`).
- **`tools` narrows and never widens.** A name the gameplan did not permit is
  refused in `resolve_agent_tools` (`crates/kerness/src/session.rs:1460`) before
  the first provider call, named with the agent. The narrowing binds the
  dispatcher as well as the prompt — a tool the agent gave up is neither
  advertised nor callable — and is outranked only by a skill's
  `requires-tools:`, a skill that agent chose to load; `Shared::active_tools`
  (`:457`) is the composition order. Under `ToolDialect::Text` every permitted
  schema is written into the system prompt, so narrowing is also what keeps
  thirty descriptions off the prompt of an agent that calls two. Tests:
  `crates/kerness/tests/tools_e2e.rs:677`, `:695`, `:713`, `:738`.
- **`skills` unions; `tools` intersects.** Deliberately asymmetric: a skill is
  capability a session offers, a tool is capability a harness grants. Both are
  tri-state — `None` takes the session's, a list selects, `[]` opts out
  (`crates/kerness/src/agent.rs:61`, `:72`).
- **`workspace` composes by intersection, not override.** `inherit` does not
  touch it; `resolve_agents` settles it against the access manager
  (`crates/kerness/src/session.rs:1540`), which can refuse it — see
  [access.md](access.md). Tests: `bindings/python/tests/test_session.py:1177`,
  `:1194`.
- **Message order is system first, then history verbatim.** `build_messages`
  (`crates/kerness/src/agent.rs:218`). Test: `the_system_prompt_leads_and_history_follows_in_order`
  (`:461`) and its Python twin (`bindings/python/tests/test_agent.py:88`).

### Extension points

- A new inheritable option is a field on `Agent`, a field on `AgentDefaults`,
  and one `if self.x.is_none()` arm in `inherit`; a new decoration is one
  `Cow::Owned` step in `decorate_system_prompt`.
- A placeholder is one entry in the substitution table at `crates/kerness/src/agent.rs:185`; the three
  are `{bot_id}`, `{bot_name}`, `{model}`.

## Key Types and Entry Points

- `crates/kerness/src/agent.rs:22` — `Agent` — name, model, reasoning effort,
  persona, role and position, language, system prompt, provider, skills, tools,
  memory scope, workspace. `new(name)` at `:84`.
- `crates/kerness/src/agent.rs:103` — `with_model(model)` / `:123`
  `with_provider(provider, model)` — the two ways to answer for an agent; the
  second takes both because a model name and a backend are a pair.
- `crates/kerness/src/agent.rs:114` — `with_role(role)` — stores the spec
  verbatim and leaves `position` alone.
- `crates/kerness/src/agent.rs:130` — `model_name()` / `:136` `effort()` — the
  resolved value or the default, never an `Option`; `effort()` is what every
  provider call reads.
- `crates/kerness/src/agent.rs:141` — `build_system_prompt(default_prompt,
  show_reasoning, skills_prompt)` — the base prompt plus every decoration;
  `Result` because a persona file may be missing.
- `crates/kerness/src/agent.rs:157` — `decorate_system_prompt(prompt, ...)` —
  persona, reasoning note, language, skills index, placeholders, in that order;
  the session calls this on an orchestrator prompt it already built.
- `crates/kerness/src/agent.rs:218` — `build_messages(history, ...)` — the
  system message followed by history; exactly what goes to the provider.
- `crates/kerness/src/agent.rs:240` — `resolve_role()` — the role file's body,
  the prose itself, or the built-in `participant` role when none was named.
- `crates/kerness/src/agent.rs:274` — `inherit(defaults)` — fills every unset
  option from `AgentDefaults` (`:313`); `Error::Session` for a provider without
  a model or a model named nowhere.
- `bindings/python/src/types.rs:645` — `PyAgent` — holds the crate `Agent` plus
  the caller's provider object so the attribute reads back as what was set;
  `build_system_prompt` at `:894` forwards; `__eq__` at `:927` compares the
  configuration.

## Interactions

- Held by [session.md](session.md), which owns the agent list, seats the chair
  in `add_agent`, inherits and confines in `resolve_agents`, narrows tools in
  `resolve_agent_tools`, and decides turn order. Contract: the agent is read
  after `inherit`, so a `None` seen mid-run means the session declared nothing
  either. Tested end to end in `crates/kerness/tests/session_run.rs`.
- Its prompt parts come from [prompting.md](prompting.md),
  [persona.md](persona.md), [memory.md](memory.md), and [skills.md](skills.md);
  `PromptAssembler` supplies them as callbacks and this module orders them.
- Its `provider` is a [provider.md](provider.md) trait object, and its
  `reasoning_effort` is that module's enum, sent per turn.
- Its `position` is [role.md](role.md)'s closed enum, read from a role file's
  frontmatter.
- Driven one turn at a time by [agent-runtime.md](agent-runtime.md), which
  borrows the agent and reads `effort()` on every call.
- Its `workspace` is settled by [access.md](access.md)'s `confine_agent`.

## How to Test

```sh
cargo test -p kerness agent::                                     # pass = 14 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_agent.py -q # pass = 14 passed
cargo test -p kerness --test session_run                          # pass = 24 passed, 0 failed
cargo test -p kerness --test tools_e2e                            # pass = 18 passed, 0 failed
```

- `crates/kerness/src/agent.rs:351` — `an_undecorated_agent_gets_the_prompt_it_was_given`
  and `:372` `every_decoration_is_appended_to_the_base`: decoration adds, never
  replaces. `bindings/python/tests/test_agent.py:12` and `:21` are the Python
  twins.
- `crates/kerness/src/agent.rs:531` — `an_unset_option_takes_the_sessions_and_a_set_one_keeps_its_own`
  — the inheritance table, option by option.
- `bindings/python/tests/test_agent.py:88` — `test_the_system_prompt_leads_and_history_follows_in_order`
  — what `build_messages` guarantees.
- `bindings/python/tests/test_agent.py:105` — `test_an_agent_on_its_own_is_always_a_participant` — and `:136`
  `test_position_is_read_only`: the chair is something a session grants, not
  something a constructor claims.
- `bindings/python/tests/test_agent.py:127` — `test_any_string_is_a_role_because_prose_is_one` — there is nothing to
  reject in a role, which is why `position` is the closed half.
- `bindings/python/tests/test_agent.py:145` — `test_the_level_is_unset_until_named_and_round_trips_as_its_name` — and
  `:158` `test_an_unknown_level_is_rejected`: `reasoning_effort` crosses the
  boundary as a validated string, the way `position` does.
- `bindings/python/tests/test_session.py:1093` — `TestSessionDefaults`: a session
  model filling the agents that named none (`:1096`), an agent on a second
  provider refused for naming no model (`:1125`), and a model named nowhere
  naming both places to write one (`:1141`). The Rust counterparts are
  `crates/kerness/tests/session_run.rs:1277`, `:1327`, `:1360`.
- `bindings/python/tests/test_session.py:815` — `TestPerAgentTools` — and
  `crates/kerness/tests/tools_e2e.rs:677`, `:695`, `:713`, `:738`: what `tools`
  narrows, and that it cannot widen. Tested through a session because narrowing
  is resolved there, not on the record.
- `crates/kerness/tests/session_run.rs:1006` — `an_agent_provider_overrides_the_session_one`
  — a per-agent provider is the one that is called.
- Gap: `AgentDefaults` having no `system_prompt` field is asserted by no test;
  the invariant rests on the struct definition.

## Review and Refactor Guide

- **Adding an inheritable option** → `Agent` (`crates/kerness/src/agent.rs:22`),
  `AgentDefaults` (`:313`), `inherit` (`:274`), the hand-written `Debug`
  (`:325`), the `PyAgent` constructor signature and its getter/setter pair
  (`bindings/python/src/types.rs:686`), and `PyAgent::__eq__` (`:927`); extend
  `an_unset_option_takes_the_sessions_and_a_set_one_keeps_its_own`
  (`crates/kerness/src/agent.rs:531`).
  The session's `AgentDefaults` construction in [session.md](session.md) must
  supply the new default.
- **Changing prompt decoration order or wording** → `decorate_system_prompt`
  (`crates/kerness/src/agent.rs:157`); dependent modules [prompting.md](prompting.md) (which measures the
  result as compaction overhead) and [role.md](role.md) (the orchestrator prompt
  is decorated by the same function); tests `every_decoration_is_appended_to_the_base`
  (`:372`) and `show_reasoning_says_yes_no_or_nothing_at_all` (`:389`).
- **Changing the provider/model pairing rule** → `inherit` (`crates/kerness/src/agent.rs:274`) and the
  four session-level tests named above; the error text names both places to
  write a model, and `a_model_named_nowhere_says_where_to_write_one`
  (`crates/kerness/tests/session_run.rs:1360`) asserts it.
- **Changing what `tools` or `skills` means** → the field docs (`crates/kerness/src/agent.rs:61`, `:72`),
  `resolve_agent_tools` (`crates/kerness/src/session.rs:1460`),
  `Shared::active_tools` (`:457`), and the four `tools_e2e.rs` cases; the
  narrowing must still bind dispatch, not only the prompt.
- **Safe extension points**: a new placeholder in the substitution table; a new
  decoration step; a new builder method mirroring `with_model`.
- **Forbidden coupling**: `agent.rs` must not import `session`, `access`, or
  `toolkit`; confinement and narrowing are the session's to apply. `with_role`
  must never set `position`.
- **Compatibility checks**: the Python constructor's keyword order at
  `bindings/python/src/types.rs:686` is public; `position` reads as a string
  (`"participant"` / `"orchestrator"`); `reasoning_effort` round-trips as its
  lowercase name (`bindings/python/tests/test_agent.py:145`).

### Improvement candidates (proposals, not accepted work)

- Assert `AgentDefaults` has exactly the four inheritable fields, so an added
  `system_prompt` fallback fails a test rather than a review; success: a unit
  test in `agent.rs` that constructs `AgentDefaults` with a struct literal
  naming every field.
- Validate a model name against the provider at `inherit` time where the
  backend can answer; success: a session with a misspelled model fails before
  the first turn in a new `session_run.rs` case. The first call is the
  only check (see Open Gaps).

## Open Gaps / Roadmap

- An agent's model is a plain string handed to its provider; there is no
  validation that the provider knows the model. What `inherit` refuses is a
  model *silently crossing a provider boundary*, which is the failure the
  framework can see; whether a given backend has heard of a given name is still
  answered by the first call.
- Per-agent memory scopes are supported, but the session's shared memory is the
  common case; two agents pointed at one scope both see each other's writes (see
  [memory.md](memory.md)).
