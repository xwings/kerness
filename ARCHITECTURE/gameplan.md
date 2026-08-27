# Gameplan

## Goal

A gameplan is a Markdown file whose YAML frontmatter is the harness contract and
whose body is the orchestrator's prose manual. This module loads one — by
built-in name or by path — splits it, hands the frontmatter to
[harness.md](harness.md), and keeps the body as the text the orchestrator is
given. Serves **M2**.

`assets.rs` is the other half: the built-in gameplans, personas, and skills that
ship with the framework, and the three-step resolution of where they live.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/gameplan.rs` | loading, splitting, and the built-in list |
| `crates/kerness/src/assets.rs` | the assets root and Markdown-stem enumeration |
| `crates/kerness/assets/gameplans/*.md` | the built-in gameplans |
| `bindings/python/src/types.rs:1673` | `PyGameplanConfig` |
| `bindings/python/kerness/gameplan_loader.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/gameplan.rs:21` — `GameplanConfig` — the parsed harness spec,
  the body text, and the path it came from.
- `crates/kerness/src/gameplan.rs:65` — `load_gameplan(name_or_path)` — a bare name
  resolves against the built-ins; anything with a separator or `.md` is a path.
- `crates/kerness/src/gameplan.rs:37` — `directory()` — the gameplan's own
  directory, which is how a gameplan can reference personas beside it.
- `crates/kerness/src/gameplan.rs:45` — `requires_orchestrator()` — whether the
  session must have an orchestrator registered before it can run.
- `crates/kerness/src/gameplan.rs:50` — `max_rounds()` — the loop bound, read from
  the harness spec.
- `crates/kerness/src/gameplan.rs:105` — `list_builtin_gameplans()` — enumerated
  from disk, not from a literal list, so an added or removed asset cannot escape
  the self-check.
- `crates/kerness/src/assets.rs:31` — `root()` — resolution order:
  `set_root()`, then `$KERNESS_ASSETS`, then `$CARGO_MANIFEST_DIR/assets`. The
  Python package calls `set_root` at import because only it knows where pip put
  the files.
- `crates/kerness/src/assets.rs:47` — `list_markdown_stems(dir)` — the shared
  enumeration behind all three `list_builtin_*` functions.

## Interactions

- Produces the `HarnessSpec` that [harness.md](harness.md) validates.
- Loaded by [session.md](session.md) at construction.
- Its directory is a persona search path for [persona.md](persona.md).
- Its built-in list is walked by [selfcheck.md](selfcheck.md), which loads every
  one rather than merely listing them.

## How to Test

```sh
cargo test -p kerness gameplan                                              # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_gameplan_loader.py -q # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_packaging.py -q       # pass = 0 failed
```

- `bindings/python/tests/test_packaging.py:30` asserts the crate's assets and the package's
  assets are byte-identical; nothing in the build enforces it.
- `bindings/python/tests/test_gameplan_loader.py:32` — `test_every_discovered_gameplan_loads_under_its_own_name` —
  the discovery assertion: every file on disk is loaded, not merely listed.
- `:28` `test_load_missing_gameplan`, `:119` `test_invalid_yaml_reports_the_file`,
  and `:141` `test_a_file_with_no_frontmatter_loads_on_harness_defaults` — a file
  with no contract is valid and takes the defaults; only malformed YAML is an
  error.

## Open Gaps / Roadmap

- The built-in gameplans stay framework-generic by project rule; a
  domain-specific gameplan belongs with the project that owns the domain, as
  `bindings/python/examples/texas_holdem/gameplan/` demonstrates.
- `load_gameplan` reads the file on every call. Sessions load once, so the
  caching that would help is only for a caller enumerating all built-ins.
- A gameplan cannot include or extend another; the contract is one file.
