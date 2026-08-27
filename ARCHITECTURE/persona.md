# Persona

## Goal

A persona is a Markdown file describing who an agent is — its voice, priorities,
and constraints. This module loads one, from the built-ins or from a path
searched relative to the gameplan, and renders it into the block that goes into
the agent's system prompt. Serves **M2**.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/persona.rs` | loading, path resolution, and prompt rendering |
| `crates/kerness/assets/personas/*.md` | the built-in personas |
| `crates/kerness-py/src/types.rs:973` | `PyPersonaConfig` |
| `python/kerness/persona_loader.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/persona.rs:17` — `PersonaConfig` — the parsed frontmatter
  fields plus the body.
- `crates/kerness/src/persona.rs:36` — `load_persona(path, search)` — `search` is
  the ordered list of directories tried before the built-ins, which is how a
  gameplan can ship personas beside it.
- `crates/kerness/src/persona.rs:89` — `resolve_persona_path(path, search)` —
  separated from loading so a caller can report which file would be used without
  reading it, and so path resolution is testable against traversal on its own.
- `crates/kerness/src/persona.rs:64` — `format_persona_for_prompt(config)` — the
  block as the agent sees it.
- `crates/kerness/src/persona.rs:79` — `list_builtin_personas()` — enumerated from
  disk, like the other asset lists.

## Interactions

- Its rendered text is one of the parts [agent.md](agent.md) builds a system
  prompt from.
- Search paths come from [gameplan.md](gameplan.md)'s `directory()`.
- Loaded for every built-in by [selfcheck.md](selfcheck.md).

## How to Test

```sh
cargo test -p kerness persona                                # pass = 0 failed
.venv/bin/python -m pytest tests/test_persona_loader.py -q   # pass = 0 failed
```

- `tests/test_persona_loader.py:111` — `test_the_working_directory_wins_over_the_search_path` —
  precedence, which is the whole reason `search` is ordered.
- `:126` — `test_the_error_lists_every_place_it_looked_and_no_more` — a missing
  persona names every candidate path, so an author can see which root was wrong.
- `:144` — `test_it_reads_the_directory_and_every_file_in_it_loads` — the
  discovery assertion over the built-ins.

## Open Gaps / Roadmap

- Personas are static text. There is no templating, so a persona that should vary
  by round has to be expressed as instructions rather than substitutions.
- The built-ins stay framework-generic by project rule; domain personas ship with
  the harness that needs them, as `examples/texas_holdem/personas/` does.
