# Harness

## Goal

The contract. A gameplan's YAML frontmatter declares who the agents are, how the
loop runs, what phases exist, which tools and skills are available, and what
fields the result must contain. This module parses that into typed specs,
validates it against what the session actually registered, and resolves the two
lists — tools and skills — that the declaration and the registration have to
agree on.

The project rule that shapes this module: **dead configuration keys are
defects.** Every field the parser accepts is validated, rendered into a prompt,
or enforced at runtime. Nothing is reserved for later.

`yaml.rs` sits underneath, and is not a detail. Frontmatter is hand-written
configuration, and how a bare scalar resolves is a behavioural decision: this
parser implements YAML **1.1**, where `no` is a boolean. Every current YAML
library implements 1.2, where `verdict_rethink: no` is the string `"no"` and the
harness parser then rejects it as "must be a boolean".

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/harness.rs` | the specs, the parser, and validation |
| `crates/kerness/src/yaml.rs` | YAML 1.1 scalar resolution over an event stream |
| `bindings/python/src/types.rs` | eight spec pyclasses, `:1312` `OrchestratorSpec` through `:1780` `Permitted` |
| `bindings/python/kerness/harness.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/harness.rs:199` — `HarnessSpec` — the whole contract:
  agents, loop, phases, tools, skills, context sources, result fields.
- `crates/kerness/src/harness.rs:312` — `parse_harness(data, source)` — frontmatter
  to spec; `source` is carried only so an error names the file.
- `crates/kerness/src/harness.rs:353` — `validate_harness(...)` — checks the spec
  against the registered agents, tools, skills, and context sources, and is where
  a contract that cannot be satisfied fails, before any provider call is made.
- `crates/kerness/src/harness.rs:342` — `Permitted` — what validation returns:
  the two narrowed lists, in registration order, that the session then works
  from.
- `crates/kerness/src/harness.rs:226` — `resolve_tools(registered)` — the declared
  tool list intersected with what exists; an undeclared name is an error, not a
  silent drop.
- `crates/kerness/src/harness.rs:242` — `resolve_context(registered)` — the same
  for context sources; see [context.md](context.md).
- `crates/kerness/src/harness.rs:296` — `resolve_skills(session_skills)` — the same
  for skills, but a session may add skills the gameplan did not name.
- `crates/kerness/src/harness.rs:119` — `LoopSpec` — rounds, phases, termination;
  `consensus_keyword()` at `:153` is what the loop scans replies for.
- `crates/kerness/src/harness.rs:163` — `ResultField` — a named output field and
  its type; `result_type()` at `:185` parses the declared type string.
- `crates/kerness/src/harness.rs:25` — `RESERVED_TOOL_NAMES` — `["Skill"]`; a
  gameplan cannot register a tool by that name because the skill runtime owns it.
- `crates/kerness/src/yaml.rs:42` — `parse(text)` — reads parser *events*, so a
  quoted `"no"` stays a string while a plain `no` becomes `false`; a `Value`-level
  deserializer has already discarded that distinction.

### Two lists narrow, one widens

`tools:` and `context:` resolve through one function, `narrow`
(`harness.rs:257`), which differs between them only in two strings: what is
being resolved, and the call that registers one. Both name something the *host
program* supplied — a handler, a function — so a gameplan naming one nobody
registered is naming nothing, and that is an error rather than a silent drop.
Silently ignoring a declared tool is how a session runs to completion doing none
of what the gameplan asked for.

`skills:` is the exception and widens: a skill is a directory of prose the
framework can load itself, so a gameplan naming one the session did not is
asking for something that can be honoured. The asymmetry is the same one
[agent.md](agent.md) draws between an agent's `tools` and its `skills`, for the
same reason.

Absence is not an empty list in any of the three. `None` means "everything
registered", and `[]` means "none", which is what makes opting a harness out of
tools entirely expressible.

## Interactions

- Parsed out of a Markdown file by [gameplan.md](gameplan.md).
- Validated against the registrations held by [session.md](session.md).
- `LoopSpec` and `PhaseSpec` drive [loop.md](loop.md).
- `ResultField` shapes the closing prompt and the parsed result in
  [loop.md](loop.md).
- Tool names are resolved against [toolkit.md](toolkit.md)'s registry, skill
  names against [skills.md](skills.md), and context source names against
  [context.md](context.md)'s.

## How to Test

```sh
cargo test -p kerness harness                                       # pass = 0 failed
cargo test -p kerness yaml                                          # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_harness.py -q # pass = 0 failed
```

- The YAML tests cover the 1.1 resolutions that differ from 1.2: `yes`/`no`/
  `on`/`off`, leading-zero octal, and an unsigned exponent that is not a number.
- `bindings/python/tests/test_harness.py:130` — `test_unknown_tool_is_an_error_not_a_silent_drop`,
  `:135` `test_reserved_tool_name_rejected_at_load`, and `:79`
  `test_a_scalar_of_the_wrong_type_is_rejected_not_coerced`: the three ways the
  contract refuses rather than guesses.
- `bindings/python/tests/test_harness.py:243` — `test_every_problem_is_reported_at_once` —
  validation collects every failure before returning, so an author fixing three
  problems runs the loader once.
- `bindings/python/tests/test_harness.py:179` — `test_passing_session_returns_what_it_permits` —
  what `Permitted` carries out of validation — with `:191`
  `test_registered_context_is_optional_and_defaults_to_none_registered`: a
  session registering no sources is only in trouble if the gameplan asked for
  one.
- `crates/kerness/src/harness.rs:1034` — `the_context_key_narrows_what_was_registered` —
  the three states of the key, and `:1055`
  `an_unknown_context_source_is_an_error_and_says_how_to_register_one` — the
  refusal naming `session.add_context(...)`.

## Open Gaps / Roadmap

- Two YAML 1.1 resolutions are deliberately dropped: a date-shaped plain scalar
  stays a string, and `.inf`/`.nan` stay strings because the JSON value model has
  no room for them. Neither is expressible in a harness field, so this is a
  bounded gap, not a pending fix (`yaml.rs:22`).
- There is no schema document for the frontmatter. The parser is the
  specification, and `crates/kerness/assets/gameplans/` is the worked example.
