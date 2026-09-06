---
eatmycode_version: "1.1.0"
---

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

This module does not own the loop that the contract bounds
([loop.md](loop.md)), the file the frontmatter is cut out of
([gameplan.md](gameplan.md)), or the registrations it validates against
([session.md](session.md)).

## Status

`done`. `cargo test -p kerness harness` passes 30 tests and
`cargo test -p kerness yaml` passes 14; `cargo test -p kerness --test
harness_contract` passes 16 integration tests; the Python boundary suite
`bindings/python/tests/test_harness.py` passes 25.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/harness.rs` | the specs, the parser, and validation |
| `crates/kerness/src/yaml.rs` | YAML 1.1 scalar resolution over an event stream |
| `bindings/python/src/types.rs` | eight spec pyclasses, `bindings/python/src/types.rs:1312` `PyOrchestratorSpec` through `bindings/python/src/types.rs:1780` `PyPermitted` |
| `bindings/python/src/funcs.rs` | `parse_harness` (`bindings/python/src/funcs.rs:293`), `validate_harness` (`bindings/python/src/funcs.rs:304`), and the `RESERVED_TOOL_NAMES` re-export (`bindings/python/src/funcs.rs:739`) |
| `bindings/python/kerness/harness.py` | re-export shim |
| `crates/kerness/tests/harness_contract.rs` | the contract driven through whole sessions |

## Language and Conventions

Rust crate module plus a PyO3 binding module and a Python re-export shim. The
root's [Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply; local facts:

- Both `harness.rs` and `yaml.rs` compile their patterns once through
  `LazyLock<Regex>` (`crates/kerness/src/harness.rs:27`,
  `crates/kerness/src/yaml.rs:28`); the `.expect("static pattern")` there is
  the crate-wide spelling for a literal that cannot fail.
- Load-time refusals are `Error::GameplanLoad`, run-time refusals
  `Error::Session` — `crates/kerness/src/harness.rs:314` against `:423`. The
  distinction is what lets a caller tell a broken file from a misconfigured
  session, and the Python map keeps it (`GameplanLoadError` versus
  `SessionError`, [errors.md](errors.md)).
- `yaml::parse` returns `Result<Value, String>` rather than the crate's
  `Result` (`crates/kerness/src/yaml.rs:42`): it is below the error enum, and
  each caller wraps the string with the file it was reading
  (`crates/kerness/src/gameplan.rs:131`, `crates/kerness/src/assets.rs:160`).
- Rendering a scalar back to text goes through `pyfmt::str` and `pyfmt::repr`
  (`crates/kerness/src/harness.rs:535`, `:732`), so a refusal quotes the value
  the way a Python caller would spell it.
- Unit tests sit inline under `#[cfg(test)]` with three helpers — `parse`,
  `ok`, `message` at `crates/kerness/src/harness.rs:829` — and sentence-style
  names. The Python tests group by parser stage (`TestParseAgents`,
  `TestParseLoop`, …) in `bindings/python/tests/test_harness.py:19` onward.
- The `HarnessSpec` and `LoopSpec` pyclasses carry
  `#[allow(clippy::too_many_arguments)]` on their constructors
  (`bindings/python/src/types.rs:1661`) because the keyword signature mirrors
  the frontmatter one key per argument, `loop` spelled `r#loop` because it is
  a Rust keyword.

## Design and Invariants

Two layers, one direction. `yaml.rs` turns text into a `serde_json::Value` and
knows nothing about harnesses; `harness.rs` turns a `Value` into typed specs and
knows nothing about files. `harness` imports only `error` and `pyfmt`; `yaml`
imports only `pyfmt`. Above them, [gameplan.md](gameplan.md) calls
`yaml::parse` and then `parse_harness` (`crates/kerness/src/gameplan.rs:91`),
and [session.md](session.md) calls `validate_harness` once at preparation
(`crates/kerness/src/session.rs:1006`).

### Load time and run time are two checks

`parse_harness` (`crates/kerness/src/harness.rs:312`) validates everything
that depends only on the file: types, bounds, slugs, reserved names, an empty
terminator list. `validate_harness` (`:353`) validates what depends on the
session — the roster against the participant bounds, the orchestrator
requirement, duplicate names, and the two narrowing lists — and collects every
problem before returning one `Error::Session` listing them (`:418`), so an
author fixing three problems runs the loader once. Enforced by
`every_problem_with_the_roster_is_reported_at_once`
(`crates/kerness/src/harness.rs:1101`) and, from Python,
`test_every_problem_is_reported_at_once`
(`bindings/python/tests/test_harness.py:243`).

### Two lists narrow, one widens

`tools:` and `context:` resolve through one function, `narrow`
(`crates/kerness/src/harness.rs:257`), which differs between them only in two
strings: what is being resolved, and the call that registers one. Both name
something the *host program* supplied — a handler, a function — so a gameplan
naming one nobody registered is naming nothing, and that is an error rather
than a silent drop. Silently ignoring a declared tool is how a session runs to
completion doing none of what the gameplan asked for.

`skills:` is the exception and widens (`resolve_skills`,
`crates/kerness/src/harness.rs:296`): a skill is a
directory of prose the framework can load itself, so a gameplan naming one the
session did not is asking for something that can be honoured. The asymmetry is
the same one [agent.md](agent.md) draws between an agent's `tools` and its
`skills`, for the same reason.

Absence is not an empty list in any of the three. `parse_name_list`
(`crates/kerness/src/harness.rs:629`)
returns `None` for an absent key and `Some(vec![])` for `[]`: `None` means
"everything registered", and `[]` means "none", which is what makes opting a
harness out of tools entirely expressible. Enforced by
`the_tools_key_narrows_what_was_registered` (`:983`) and
`the_context_key_narrows_what_was_registered` (`:1034`).

### Every key is consumed

The rule the module exists for, and where each key lands:

| Key | Consumed at |
| --- | --- |
| `name` | every refusal message; `crates/kerness/src/session.rs:797` names the gameplan an agent was refused against |
| `description` | the orchestrator prompt, `crates/kerness/src/session.rs:1381` |
| `agents.orchestrator.required` / `.instruction` | `validate_harness` (`crates/kerness/src/harness.rs:362`); the orchestrator prompt, `crates/kerness/src/session.rs:1388` |
| `agents.participants.min` / `.max` | `validate_harness` (`crates/kerness/src/harness.rs:370`) |
| `loop.*` | `OrchestratorLoop::new`, `crates/kerness/src/session.rs:1116` — see [loop.md](loop.md) |
| `tools`, `context` | `narrow` (`crates/kerness/src/harness.rs:257`) |
| `skills` | `resolve_skills` (`crates/kerness/src/harness.rs:296`), called from `crates/kerness/src/session.rs:1431` |
| `result` | the closing prompt and strict validation, `crates/kerness/src/session/outcome.rs:53` |

A key added to `parse_harness` without a row here is the defect the project
rule names; no test enforces the table itself.

### Scalars resolve by YAML 1.1, from events

`yaml::parse` (`crates/kerness/src/yaml.rs:42`) reads parser *events* rather
than a `Value` tree, and `resolve_scalar` (`:211`) resolves a scalar only when
its style is plain: `"no"` in quotes stays a string while a bare `no` becomes
`false`. A `Value`-level deserializer has already discarded that distinction
by the time anything can look at it. `parse_bool` in the harness parser
(`crates/kerness/src/harness.rs:744`) then requires an actual boolean rather
than truth-testing whatever arrived, and `coerce_int` (`:759`) refuses a bool
so `max_rounds: true` cannot quietly mean one round. Enforced by
`yaml_one_point_one_booleans_are_still_booleans` and
`quoting_is_what_keeps_a_word_a_word` (`crates/kerness/src/yaml.rs:374`,
`:382`) and `a_scalar_of_the_wrong_type_is_rejected_not_coerced`
(`crates/kerness/src/harness.rs:935`).

Two 1.1 resolutions are deliberately dropped: a date-shaped plain scalar stays
a string, and `.inf`/`.nan` stay strings because the JSON value model has no
room for them (`crates/kerness/src/yaml.rs:22`). A stream holding more than
one document is refused rather than silently choosing one (`:57`).

### Invariants a change must preserve

- A `LoopSpec` always has at least one terminator: `[]` is refused at load
  (`crates/kerness/src/harness.rs:527`), because a harness with no way to
  stop is worth refusing at load rather than at turn `max_turns`. Enforced by
  `a_single_terminator_is_wrapped_and_none_at_all_is_refused` (`:908`) and,
  for every bundled gameplan, by
  `every_bundled_gameplan_loads_and_declares_a_terminator`
  (`crates/kerness/tests/public_api.rs:178`).
- `RESERVED_TOOL_NAMES` (`crates/kerness/src/harness.rs:25`) cannot appear in
  `tools:`; the skill runtime builds `Skill` per agent
  ([skills.md](skills.md)). Enforced by
  `a_reserved_tool_name_is_rejected_at_load` (`crates/kerness/src/harness.rs:1015`).
- `Permitted` (`crates/kerness/src/harness.rs:342`) returns both lists in
  *registration* order, never declaration order, so the prompt and the
  dispatcher agree on ordering with the host program. Enforced by
  `a_passing_session_returns_what_it_may_use`
  (`crates/kerness/src/harness.rs:1086`).
- Every bound has a floor and the floors differ on purpose: `1` for rounds
  and turns, `0` for `orchestrator_retries` (`parse_int_at_least`,
  `crates/kerness/src/harness.rs:719`), because "do not re-ask" is a real
  choice.
- `result_type` accepts exactly the spellings in `RESULT_TYPES`
  (`crates/kerness/src/harness.rs:31`); `ResultField::result_type` (`:185`)
  falls back to `Str` only for a field built directly rather than parsed,
  since an unknown spelling is refused at load (`:697`).

## Key Types and Entry Points

- `crates/kerness/src/harness.rs:199` — `HarnessSpec` — the whole contract:
  agents, loop, phases, tools, skills, context sources, result fields.
  `Default` is a usable bare spec (`a_bare_spec_is_usable`, `:1144`).
- `crates/kerness/src/harness.rs:312` — `parse_harness(data, source)` —
  frontmatter to spec; `source` is carried only so an error names the file.
  Fails with `Error::GameplanLoad`.
- `crates/kerness/src/harness.rs:353` — `validate_harness(spec, participants,
  orchestrator, registered_tools, registered_context)` — checks the spec
  against the session and returns `Permitted`; fails once with every problem
  listed, as `Error::Session`.
- `crates/kerness/src/harness.rs:342` — `Permitted` — the two narrowed lists,
  named rather than positional, so a caller cannot read one for the other.
- `crates/kerness/src/harness.rs:226` — `resolve_tools(registered)` and `:242`
  `resolve_context(registered)` — the declared list intersected with what
  exists; an undeclared name is an error naming the registering call.
- `crates/kerness/src/harness.rs:296` — `resolve_skills(session_skills)` — the
  union, order preserved, duplicates dropped; infallible.
- `crates/kerness/src/harness.rs:119` — `LoopSpec` — turns, rounds,
  terminators, phases, `advance_on`, retries, `verdict_rethink`;
  `consensus_keyword()` at `:153` is the terminator containing `CONSENSUS`.
- `crates/kerness/src/harness.rs:163` — `ResultField` — a named output field
  and its type; `result_type()` at `:185` parses the declared spelling.
- `crates/kerness/src/harness.rs:25` — `RESERVED_TOOL_NAMES` — `["Skill"]`.
- `crates/kerness/src/yaml.rs:42` — `parse(text)` — one document to a
  `Value`; `Result<Value, String>`, an empty document is `Null`.

## Interactions

- [gameplan.md](gameplan.md) splits the Markdown file, calls `yaml::parse` on
  the frontmatter and `parse_harness` on the result
  (`crates/kerness/src/gameplan.rs:91`); the shared boundary is the
  `HarnessSpec` value. Tested end to end by
  `crates/kerness/tests/harness_contract.rs`.
- [session.md](session.md) calls `validate_harness` at preparation
  (`crates/kerness/src/session.rs:1006`) with the roster and the registered
  tool and context names, and `resolve_skills` with the session's skill list
  (`:1431`). `Permitted` is what the session narrows its dispatcher and
  context cache from.
- [loop.md](loop.md) consumes `LoopSpec` and `PhaseSpec` through
  `OrchestratorLoop::new` and `ResultField` through `with_result_fields`
  (`crates/kerness/src/session.rs:1116`).
- [run.md](run.md)'s strict result validation reads `ResultField::result_type`
  (`crates/kerness/src/session/outcome.rs:53`).
- Tool names are resolved against [toolkit.md](toolkit.md)'s registry, skill
  names against [skills.md](skills.md), and context source names against
  [context.md](context.md)'s.
- `yaml::parse` is also the parser under every other frontmatter file —
  roles, personas, and skills — through `crates/kerness/src/assets.rs:160`.
- The Python side crosses the spec as eight frozen pyclasses; `PyHarnessSpec`
  and `PyLoopSpec` constructors mirror the keys with the parser's defaults
  (`bindings/python/src/types.rs:1476`), which
  `bindings/python/tests/test_harness.py:53` `test_defaults` holds to the
  Rust `Default`.

## How to Test

```sh
cargo test -p kerness harness                                       # pass = 30 passed
cargo test -p kerness yaml                                          # pass = 14 passed
cargo test -p kerness --test harness_contract                       # pass = 16 passed
.venv/bin/python -m pytest bindings/python/tests/test_harness.py -q # pass = 25 passed
```

- `crates/kerness/src/yaml.rs:374` `yaml_one_point_one_booleans_are_still_booleans`,
  `:382` `quoting_is_what_keeps_a_word_a_word`, `:397`
  `numbers_follow_the_one_point_one_rules`, and `:407`
  `an_unsigned_exponent_is_not_a_number` — the 1.1 resolutions that differ
  from 1.2; `:457` `a_stream_with_two_documents_is_refused`.
- `crates/kerness/src/harness.rs:935`
  `a_scalar_of_the_wrong_type_is_rejected_not_coerced`, `:1006`
  `an_unknown_tool_is_an_error_not_a_silent_drop`, and `:1015`
  `a_reserved_tool_name_is_rejected_at_load` — the three ways the contract
  refuses rather than guesses; the same three from Python at
  `bindings/python/tests/test_harness.py:79`, `:130`, `:135`.
- `crates/kerness/src/harness.rs:1034`
  `the_context_key_narrows_what_was_registered` — the three states of the
  key — and `:1055`
  `an_unknown_context_source_is_an_error_and_says_how_to_register_one` — the
  refusal naming `session.add_context(...)`.
- `crates/kerness/src/harness.rs:1101`
  `every_problem_with_the_roster_is_reported_at_once` and
  `bindings/python/tests/test_harness.py:243`
  `test_every_problem_is_reported_at_once` — validation collects before
  returning.
- `bindings/python/tests/test_harness.py:179`
  `test_passing_session_returns_what_it_permits` — what `Permitted` carries
  out — with `:191`
  `test_registered_context_is_optional_and_defaults_to_none_registered`: a
  session registering no sources is only in trouble if the gameplan asked for
  one.
- `crates/kerness/tests/harness_contract.rs:192`
  `every_unmet_requirement_is_reported_at_once`, `:262`
  `a_declared_tool_that_is_not_registered_is_refused_with_the_list`, `:286`
  `a_gameplan_narrows_the_registered_tools`, `:410`
  `gameplan_skills_union_with_the_sessions`, `:457`
  `a_gameplan_claiming_a_reserved_tool_name_fails_to_load`, `:473`
  `a_phase_cannot_outlast_max_rounds`, and `:518`
  `only_the_declared_terminator_ends_the_session` — the contract seen through
  a configured session.
- Gap: the "every key is consumed" table in Design and Invariants has no
  test; a new key that parses and is never read is caught by review only.

## Review and Refactor Guide

- **Adding a frontmatter key** → add the field to `HarnessSpec`
  (`crates/kerness/src/harness.rs:199`) or
  the sub-spec, parse it in `parse_harness` (`:312`) or its helper with the
  `_ in {source}` refusal wording, consume it somewhere named in the
  Design and Invariants table, mirror it on the pyclass constructor in
  `bindings/python/src/types.rs` with the same default, and add a case to the
  parser test that owns that section. A key with no consumer is a defect.
- **Changing a default** → `LoopSpec::default`
  (`crates/kerness/src/harness.rs:137`) or `ParticipantSpec`
  (`:70`), the matching `#[pyo3(signature)]` default in
  `bindings/python/src/types.rs:1476`, `loop_defaults_include_the_judges_rethink`
  (`crates/kerness/src/harness.rs:893`), and `test_defaults` (`bindings/python/tests/test_harness.py:53`).
  The bundled gameplans under `crates/kerness/assets/gameplans/` are the
  worked examples and load in `crates/kerness/tests/public_api.rs:178`.
- **Changing a refusal message** → the Python tests match on wording
  (`bindings/python/tests/test_harness.py:130`, `:135`) and so does
  `crates/kerness/src/harness.rs:1055`; update both.
- **Changing what narrows or widens** → `narrow`
  (`crates/kerness/src/harness.rs:257`) and
  `resolve_skills` (`:296`) are the only two bodies; [agent.md](agent.md)'s
  per-agent `tools`/`skills` and [skills.md](skills.md)'s `admit_required`
  compose on top and assume this direction.
- **Touching `yaml.rs`** → every frontmatter file in the crate goes through
  it (roles, personas, skills, gameplans). Run the four asset loaders' tests
  as well as `yaml`.
- Safe extension points: `RESULT_TYPES` (`crates/kerness/src/harness.rs:31`)
  for a new result spelling,
  `parse_name_list` (`:629`) for a new tri-state list.
- Forbidden coupling: `harness.rs` must not import `session`, `gameplan`, or
  `orchestrator`; it is validated against plain name lists precisely so it can
  be tested with none of them.
- Compatibility: `Permitted`, `HarnessSpec`, and every sub-spec are public on
  both surfaces; a field rename is a Python constructor change.

Improvement candidates (proposals, not accepted work):

- A test that enumerates the keys `parse_harness` reads and asserts each has a
  consumer would turn the "every key is consumed" rule from review into
  evidence. Success check: adding a parsed-but-unread key fails the suite.
- `narrow`'s refusal lists registered names as a comma-joined string; a
  structured error would let a caller act on it, but the crate's error enum
  carries strings ([errors.md](errors.md) records that gap), so this waits on
  that decision.

## Open Gaps / Roadmap

- Two YAML 1.1 resolutions are deliberately dropped: a date-shaped plain scalar
  stays a string, and `.inf`/`.nan` stay strings because the JSON value model
  has no room for them. Neither is expressible in a harness field, so this is
  a bounded gap, not a pending fix (`crates/kerness/src/yaml.rs:22`).
- There is no schema document for the frontmatter. The parser is the
  specification, and `crates/kerness/assets/gameplans/` is the worked example.
- Nothing asserts that every parsed key is consumed; see the Review and
  Refactor Guide.
