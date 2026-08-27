# Harness

## Goal

The contract. A gameplan's YAML frontmatter declares who the agents are, how the
loop runs, what phases exist, which tools and skills are available, and what
fields the result must contain. This module parses that into typed specs,
validates it against what the session actually registered, and resolves the two
lists — tools and skills — that the declaration and the registration have to
agree on. Serves **M2**.

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
| `crates/kerness-py/src/types.rs` | seven spec pyclasses (`:1179`–`:1516`) |
| `python/kerness/harness.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/harness.rs:199` — `HarnessSpec` — the whole contract:
  agents, loop, phases, tools, skills, result fields.
- `crates/kerness/src/harness.rs:272` — `parse_harness(data, source)` — frontmatter
  to spec; `source` is carried only so an error names the file.
- `crates/kerness/src/harness.rs:303` — `validate_harness(...)` — checks the spec
  against the registered agents, tools, and skills, and is where a contract that
  cannot be satisfied fails, before any provider call is made.
- `crates/kerness/src/harness.rs:223` — `resolve_tools(registered)` — the declared
  tool list intersected with what exists; an undeclared name is an error, not a
  silent drop.
- `crates/kerness/src/harness.rs:256` — `resolve_skills(session_skills)` — the same
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

## Interactions

- Parsed out of a Markdown file by [gameplan.md](gameplan.md).
- Validated against the registrations held by [session.md](session.md).
- `LoopSpec` and `PhaseSpec` drive [loop.md](loop.md).
- `ResultField` shapes the closing prompt and the parsed result in
  [loop.md](loop.md).
- Tool names are resolved against [toolkit.md](toolkit.md)'s registry and skill
  names against [skills.md](skills.md).

## How to Test

```sh
cargo test -p kerness harness                        # pass = 0 failed
cargo test -p kerness yaml                           # pass = 0 failed
.venv/bin/python -m pytest tests/test_harness.py -q  # pass = 0 failed
```

- The YAML tests cover the 1.1 resolutions that differ from 1.2: `yes`/`no`/
  `on`/`off`, leading-zero octal, and an unsigned exponent that is not a number.
- `tests/test_harness.py:129` — `test_unknown_tool_is_an_error_not_a_silent_drop`,
  `:134` `test_reserved_tool_name_rejected_at_load`, and `:78`
  `test_a_scalar_of_the_wrong_type_is_rejected_not_coerced`: the three ways the
  contract refuses rather than guesses.
- `tests/test_harness.py:221` — `test_every_problem_is_reported_at_once` —
  validation collects every failure before returning, so an author fixing three
  problems runs the loader once.

## Open Gaps / Roadmap

- Two YAML 1.1 resolutions are deliberately dropped: a date-shaped plain scalar
  stays a string, and `.inf`/`.nan` stay strings because the JSON value model has
  no room for them. Neither is expressible in a harness field, so this is a
  bounded gap, not a pending fix (`yaml.rs:22`).
- There is no schema document for the frontmatter. The parser is the
  specification, and `crates/kerness/assets/gameplans/` is the worked example.
