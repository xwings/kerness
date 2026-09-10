---
eatmycode_version: "1.2.0"
---

# Persona

## Goal

A persona is a Markdown file describing who an agent is — its voice, priorities,
and constraints. This module owns loading one, from the built-ins or from a path
searched relative to the gameplan, and rendering it into the block that goes
into the agent's system prompt.

It does not own where a persona sits in an agent's prompt, which is
[agent.md](agent.md)'s `decorate_system_prompt`, nor the search path a session
supplies, which is [session.md](session.md)'s `resolve_personas`. A persona and a
[role](role.md) coexist and divide cleanly:

| | answers | consumed by |
| --- | --- | --- |
| `role` | *what is your position and job in this session* | the loop — dispatch, prompt base, turn order |
| `persona` | *who are you — background, expertise, voice* | the prompt only |

An agent can be the orchestrator and a devil's advocate at once. Nothing a
persona says can change where its agent sits.

## Status

`done` — `cargo test -p kerness persona` passes 13 tests and
`.venv/bin/python -m pytest bindings/python/tests/test_persona_loader.py -q`
passes 8. Both bundled personas load and render through
`crates/kerness/tests/public_api.rs:219` and the self-check.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/persona.rs` | `PersonaConfig`, loading, section extraction, path resolution, prompt rendering |
| `crates/kerness/src/assets.rs` | the shared root, candidate order, and `resolve_path` every asset family uses |
| `crates/kerness/assets/personas/*.md` | the two built-in personas |
| `bindings/python/src/types.rs:1027` | `PyPersonaConfig`, a `get_all, set_all` pyclass |
| `bindings/python/src/funcs.rs:372` | four `#[pyfunction]`s: the three loaders and the renderer |
| `bindings/python/kerness/persona_loader.py` | re-export shim |
| `bindings/python/kerness/personas/*.md` | the byte-identical installed copies |

## Language and Conventions

One Rust crate module, a pyclass and four pyfunctions in the PyO3 binding, and a
Python shim of docstring plus imports plus `__all__`. The root's [Coding Style
and Code Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply.
Local facts:

- The title regex is a `LazyLock<Regex>` with `expect("static pattern")`
  (`crates/kerness/src/persona.rs:41`), the crate's convention for a pattern
  that cannot fail at runtime. Section extraction is a line scan, not a regex,
  because the section ends at a lookahead the `regex` crate cannot express
  (`:91`).
- Unit tests use `crate::testing::TempDir` (`crates/kerness/src/persona.rs:113`)
  and sentence-style names. Python tests write files with `tempfile` and
  `tmp_path`, and the discovery test reads the installed package directory
  (`bindings/python/tests/test_persona_loader.py:144`).
- `PersonaConfig` derives `Default` so a test can name one field
  (`crates/kerness/src/persona.rs:16`); the pyclass mirrors that with
  keyword defaults of `""` (`bindings/python/src/types.rs:1055`). Observed, not
  enforced.

## Design and Invariants

### Files, resolution, and rendering are three functions

`load_persona` reads and parses; `resolve_persona_path` answers where a name
would land without reading it; `format_persona_for_prompt` renders a parsed
persona. The split is what lets a caller report which file would be used before
committing to it, and lets path resolution be tested against traversal on its
own. The module imports only `assets` and `error`; nothing in it knows about
agents or sessions.

### Search order is the caller's

`resolve_persona_path(path, search)` tries the path as written, then each
`search` directory, then the built-ins, through `assets::candidates`
(`crates/kerness/src/assets.rs:79`). The session passes the gameplan's own
directory, so a third-party project can ship a gameplan and its personas side by
side and have the paths in that gameplan resolve from any working directory. An
absolute path means one place and is tried once, so a not-found error lists each
candidate exactly once (`the_not_found_error_names_every_directory_tried`,
`crates/kerness/src/persona.rs:179`).

### A missing file fails, and it fails before the run

A `.md` persona that does not resolve is `Error::NotFound`, which crosses to
Python as `FileNotFoundError`. Passing the unresolved path through as prose
would put the literal line `Persona: ./personas/typo.md` into a system prompt and
let the run look healthy while costing real provider calls
(`Agent::resolve_persona`, `crates/kerness/src/agent.rs:208`). The session pins
every `.md` persona to an absolute path before the first turn
(`Session::resolve_personas`, `crates/kerness/src/session.rs:1555`), so
`resolve_persona` stays a plain lookup with no search path of its own, and a
`chdir` mid-run cannot change which file is read.

### Only set sections reach the prompt

`format_persona_for_prompt` emits a `Persona:`, `Background:`, or
`Communication style:` line only for a non-empty section
(`crates/kerness/src/persona.rs:64`), so a persona that only sets a style does
not spend prompt on two empty labels. A persona with nothing set renders the
empty string. Enforced by `only_the_sections_that_are_set_reach_the_prompt`
(`:151`) and the Python counterpart at
`bindings/python/tests/test_persona_loader.py:68`.

### The prompt names the character, never the path

Once personas resolve to absolute paths, a roster line like
`- Alice (/home/…/pragmatic_engineer.md)` is a private filesystem path handed to
a model. `persona_label` (`crates/kerness/src/session.rs:1910`) reads the file's
`# Persona:` title instead, and prose personas pass through as written. Enforced
by `the_roster_names_the_character_never_the_path`
(`crates/kerness/src/session.rs:2828`).

### Built-ins are enumerated, not listed

`list_builtin_personas` reads the directory (`assets::list_markdown_stems`,
`crates/kerness/src/assets.rs:54`), so a persona added to or removed from
`assets/` cannot escape `the_built_ins_are_enumerated_from_disk`
(`crates/kerness/src/persona.rs:191`) or the self-check. The crate's and the
package's copies must stay byte-identical; only
`bindings/python/tests/test_packaging.py:42` keeps them so.

## Key Types and Entry Points

- `crates/kerness/src/persona.rs:17` — `PersonaConfig` — the `# Persona:` title
  (or the file's stem) and the three section bodies, each `""` when absent.
- `crates/kerness/src/persona.rs:36` — `load_persona(path, search)` — resolve,
  read, parse. `Error::NotFound` when nothing resolves, `Error::Io` naming the
  path when the read fails.
- `crates/kerness/src/persona.rs:64` — `format_persona_for_prompt(config)` — the
  block as the agent sees it; one line per set section, joined by newlines.
- `crates/kerness/src/persona.rs:82` — `list_builtin_personas()` — sorted stems
  of `assets/personas/*.md`, read from disk on every call.
- `crates/kerness/src/persona.rs:87` — `resolve_persona_path(path, search)` —
  the first existing candidate, or `Error::NotFound` naming every directory
  tried.
- `crates/kerness/src/persona.rs:96` — `extract_section(text, heading)` — the
  body under one `## <heading>` up to the next `## `, trimmed; `""` when the
  heading is absent.
- `crates/kerness/src/agent.rs:208` — `Agent::resolve_persona()` — the one
  consumer: a `.md` persona is loaded and rendered, prose becomes
  `Persona: <text>`.
- `bindings/python/src/funcs.rs:372` — `load_persona(path, search=None)` — the
  pyfunction; `:380` `format_persona_for_prompt`, `:386`
  `list_builtin_personas`, `:393` `resolve_persona_path` returning a `Path`.

## Interactions

- [agent.md](agent.md) — `decorate_system_prompt`
  (`crates/kerness/src/agent.rs:157`) calls `resolve_persona` and places the
  rendered block after the base prompt. The contract is the rendered string;
  tested at `bindings/python/tests/test_agent.py:50` and `:78`.
- [session.md](session.md) — `resolve_personas`
  (`crates/kerness/src/session.rs:1555`) supplies the search path from
  [gameplan.md](gameplan.md)'s `directory()` and writes absolute paths back onto
  each agent; `persona_label` (`:1910`) reads the title for the orchestrator's
  roster. Tested at `bindings/python/tests/test_session.py:2773`.
- [gameplan.md](gameplan.md) — shares `assets::resolve_path`, `candidates`, and
  `list_markdown_stems` with every other asset family.
- [selfcheck.md](selfcheck.md) — loads every built-in
  (`bindings/python/kerness/selfcheck.py:105`).
- [errors.md](errors.md) — `Error::NotFound` crosses as `FileNotFoundError`,
  which `bindings/python/tests/test_persona_loader.py:63` asserts.

## How to Test

```sh
cargo test -p kerness persona                                              # pass = 13 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_persona_loader.py -q # pass = 8 passed
cargo test -p kerness --test public_api every_bundled_persona              # pass = 1 passed
```

- `crates/kerness/src/persona.rs:129` —
  `every_section_is_parsed_and_the_title_wins_over_the_stem` — and `:141`
  `a_file_with_no_title_is_named_for_itself`: the parse, including that an
  absent section reads as empty rather than carrying the next section's prose.
- `crates/kerness/src/persona.rs:164` — `a_bare_name_resolves_to_the_built_ins`
  — `:170` `a_search_directory_is_tried_before_the_built_ins`, and `:179`
  `the_not_found_error_names_every_directory_tried`: the resolution order and
  its error.
- `bindings/python/tests/test_persona_loader.py:111` —
  `test_the_working_directory_wins_over_the_search_path` — precedence seen from
  Python, which is why `search` is ordered.
- `bindings/python/tests/test_persona_loader.py:126` —
  `test_the_error_lists_every_place_it_looked_and_no_more` — an absolute path
  is reported once.
- `bindings/python/tests/test_persona_loader.py:144` —
  `test_it_reads_the_directory_and_every_file_in_it_loads` — the discovery
  assertion over the installed built-ins.
- `bindings/python/tests/test_agent.py:69` —
  `test_a_missing_persona_file_raises_and_names_what_it_tried` — the fail-early
  rule at the agent, and `bindings/python/tests/test_session.py:2794`
  `test_a_missing_persona_stops_the_run_before_it_costs_anything` at the
  session.
- Gap: `extract_section` has no test for a heading followed by trailing
  whitespace, which the `is_some_and(all whitespace)` check at
  `crates/kerness/src/persona.rs:100` accepts.

## Review and Refactor Guide

- Changing the file format (a new section, a different title marker) →
  `extract_section` and `load_persona` (`crates/kerness/src/persona.rs:96`,
  `:36`), `PersonaConfig` and `PyPersonaConfig` (`bindings/python/src/types.rs:1027`,
  whose `__eq__` and constructor list every field), both built-in files, and the
  parse tests at `crates/kerness/src/persona.rs:129`.
- Changing the rendered block → `format_persona_for_prompt` only; the decoration
  tests at `bindings/python/tests/test_prompting.py:127` and
  `bindings/python/tests/test_agent.py:21` assert the `Persona:` prefix.
- Changing resolution order → `assets::candidates`
  (`crates/kerness/src/assets.rs:79`) is shared with [role.md](role.md); change
  both docs and both search tests.
- Safe extension point: a new asset family reuses `assets::resolve_path` and
  `list_markdown_stems` rather than a private resolver.
- Forbidden coupling: this module must not import `agent`, `session`, or
  `prompting`; it is a leaf that [agent.md](agent.md) calls.
- Compatibility: `PyPersonaConfig` is constructed by keyword from Python
  (`bindings/python/tests/test_persona_loader.py:72`), so a renamed field is a
  public change. Bundled personas stay framework-generic by project rule.

Improvement candidates (proposals, not accepted work):

- Cache a loaded persona on the agent. Benefit: `resolve_persona` reads the
  file on every prompt assembly. Check: one read per agent per run in a
  scripted session, and `test_a_persona_beside_the_gameplan_resolves_from_any_cwd`
  (`bindings/python/tests/test_session.py:2810`) still passes.

## Open Gaps / Roadmap

- Personas are static text. There is no templating, so a persona that should
  vary by round has to be expressed as instructions rather than substitutions.
- The built-ins stay framework-generic by project rule; domain personas ship
  with the harness that needs them, as
  `bindings/python/examples/texas_holdem/personas/` does.
- `load_persona` reads the file on every call and the agent re-reads it on
  every prompt; see the improvement candidate above.
