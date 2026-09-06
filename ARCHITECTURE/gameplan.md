---
eatmycode_version: "1.1.0"
---

# Gameplan

## Goal

A gameplan is a Markdown file whose YAML frontmatter is the harness contract and
whose body is the orchestrator's prose manual. This module loads one — by
built-in name or by path — splits it, hands the frontmatter to
[harness.md](harness.md), and keeps the body as the text the orchestrator is
given.

`assets.rs` is the other half: where the built-in gameplans, roles, personas,
and skills live, the three-step resolution of that root, and the reading every
asset family shares. This module does not validate the contract, resolve
personas, or decide what the orchestrator does with the body.

## Status

`done` — `cargo test -p kerness gameplan` passes 23 tests,
`bindings/python/tests/test_gameplan_loader.py` passes 13, and
`bindings/python/tests/test_packaging.py` passes 4.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/gameplan.rs` | loading, splitting, and the built-in list |
| `crates/kerness/src/assets.rs` | the assets root, Markdown-stem enumeration, path candidates, and the shared frontmatter splitter |
| `crates/kerness/assets/gameplans/*.md` | the three built-in gameplans |
| `bindings/python/kerness/gameplans/*.md` | the same three, byte-identical, installed with the package |
| `bindings/python/src/types.rs:1852` | `PyGameplanConfig` |
| `bindings/python/src/funcs.rs:326` | `load_gameplan` and `list_builtin_gameplans` |
| `bindings/python/kerness/gameplan_loader.py` | re-export shim |

## Language and Conventions

Two Rust crate modules, a frozen pyclass and two pyfunctions in the binding,
and a Python re-export shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `gameplan.rs` depends on `assets`, `error`, `harness`, and `yaml`;
  `assets.rs` on `error`, `pyfmt`, and `yaml`. Every failure is
  `Error::GameplanLoad` here (`crates/kerness/src/gameplan.rs:74`, `:132`,
  `:141`, `:178`, `:185`) and `Error::NotFound` or `Error::Value` in
  `assets.rs` (`crates/kerness/src/assets.rs:111`, `:161`, `:168`), so a
  missing gameplan is a framework exception while a missing persona is
  Python's `FileNotFoundError` ([errors.md](errors.md)).
- The assets root is a global `OnceLock<RwLock<Option<PathBuf>>>` slot
  (`crates/kerness/src/assets.rs:27`), the same pattern as the four seams in
  the root's table; `set_root` is what `bootstrap` calls
  (`bindings/python/src/lib.rs:43`).
- The frontmatter regex is a `LazyLock<Regex>` with `expect("static pattern")`
  (`crates/kerness/src/gameplan.rs:118`), the crate's convention for compiled
  constants.
- `PyGameplanConfig` is `frozen` (`bindings/python/src/types.rs:1852`) with
  getters only; `directory` returns a Python path or `None`.
- Unit tests write scratch files through a local `TempFile`
  (`crates/kerness/src/gameplan.rs:198`) rather than the crate's `TempDir`,
  because each test needs exactly one file; the Python tests use
  `tempfile.NamedTemporaryFile`. `test_every_gameplan_has_think_and_rethink`
  is parametrised over `list_builtin_gameplans()` at collection time
  (`bindings/python/tests/test_gameplan_loader.py:65`).

## Design and Invariants

### Two loaders, one root

`load_gameplan` (`crates/kerness/src/gameplan.rs:70`) resolves a name or path,
reads the file, splits the frontmatter with its own regex (`:117`), parses the
YAML through [harness.md](harness.md)'s 1.1 parser, and hands the mapping to
`parse_harness`. `assets::split_frontmatter` (`crates/kerness/src/assets.rs:139`)
is the line-based splitter the role, persona, and skill loaders share; it is
not used here because the gameplan's closing delimiter tolerates trailing
whitespace and the others require an exact `---` line. The two splitters are a
known duplication with a documented reason (`assets.rs:130`).

The root is resolved in three steps and the first answer wins
(`crates/kerness/src/assets.rs:38`): `set_root`, then `$KERNESS_ASSETS`, then
`$CARGO_MANIFEST_DIR/assets`. The assets are files on disk rather than strings
compiled into the binary because a skill directory is also a bundle whose
frontmatter can grant read access to files beside it, and a path that only
exists inside the executable cannot be granted, listed, or read
([skills.md](skills.md)).

### Invariants

- **A bare name is a built-in; a separator or `.md` is a path.**
  `resolve_gameplan_path` (`crates/kerness/src/gameplan.rs:165`) tries a path
  verbatim, then under the built-in directory, and refuses with the name it
  was given; a bare name resolves to `<root>/gameplans/<name>.md`. Enforced by
  `a_missing_gameplan_is_named_in_the_error` (`:236`) and
  `a_custom_gameplan_loads_from_an_absolute_path` (`:328`).
- **The body is the prose and never the YAML.** `body` is the text after the
  frontmatter, trimmed; `raw_text` is the whole file. Enforced by
  `the_body_is_the_prose_and_raw_text_is_the_whole_file` (`:219`) and its
  Python twin (`bindings/python/tests/test_gameplan_loader.py:13`).
- **No frontmatter is a valid gameplan on harness defaults.** A file that
  does not start with the delimiter loads with an empty mapping and the whole
  file as body (`crates/kerness/src/gameplan.rs:122`); a `null` mapping is
  treated the same (`:135`); a non-mapping is refused with its type (`:140`).
  Enforced by `a_file_with_no_frontmatter_loads_on_harness_defaults` (`:361`)
  and `invalid_yaml_reports_the_file` (`:354`).
- **A filename-derived name is not slug-validated.** The declared `name:` goes
  through `parse_harness`; a name filled in from the stem does not, because
  the author did not choose it as an identifier (`:89`). Enforced by
  `an_undeclared_name_falls_back_to_the_filename` (`:342`).
- **The stored path is canonical.** `path` is canonicalised when possible
  (`:100`), so `directory()` keeps meaning the same place after a `chdir`, and
  sibling personas and roles resolve against it
  (`crates/kerness/src/session.rs:739`, `:1556`).
- **Built-ins are enumerated, never listed.** `list_builtin_gameplans` reads
  the directory (`crates/kerness/src/assets.rs:54`) and sorts; an unreadable
  directory lists as empty. Enforced by
  `every_discovered_gameplan_loads_under_its_own_name`
  (`crates/kerness/src/gameplan.rs:244`), `stems_are_listed_from_disk_and_sorted`
  (`crates/kerness/src/assets.rs:189`), and the same discovery from Python
  (`bindings/python/tests/test_gameplan_loader.py:32`).
- **Every built-in declares a terminator and think/rethink phases.** Enforced
  by `every_gameplan_has_think_and_rethink` (`crates/kerness/src/gameplan.rs:293`),
  `every_bundled_gameplan_loads_and_declares_a_terminator`
  (`crates/kerness/tests/public_api.rs:178`), and
  `bindings/python/kerness/selfcheck.py:74` at install time.
- **The two asset copies are byte-identical.** Nothing in the build keeps
  `crates/kerness/assets/` and `bindings/python/kerness/` in step; enforced by
  `test_the_crate_and_the_package_ship_the_same_assets`
  (`bindings/python/tests/test_packaging.py:42`).

## Key Types and Entry Points

- `crates/kerness/src/gameplan.rs:21` — `GameplanConfig` — `name`, the parsed
  `HarnessSpec`, the `body` the orchestrator reads, the `raw_text`, and the
  canonical `path`.
- `crates/kerness/src/gameplan.rs:37` — `directory()` — the gameplan's own
  directory, or `None` for a config with no path; the persona and role search
  root.
- `crates/kerness/src/gameplan.rs:50` — `requires_orchestrator()` / `:55`
  `max_rounds()` — shorthands over the harness spec the session reads at
  construction.
- `crates/kerness/src/gameplan.rs:70` — `load_gameplan(name_or_path)` — read,
  split, parse; every failure is `Error::GameplanLoad` naming the file.
- `crates/kerness/src/gameplan.rs:109` — `list_builtin_gameplans()` — sorted
  stems of `<root>/gameplans/*.md`.
- `crates/kerness/src/assets.rs:33` — `set_root(path)` / `:38` `root()` — the
  override and the three-step resolution.
- `crates/kerness/src/assets.rs:54` — `list_markdown_stems(dir)` — the shared
  enumeration behind every `list_builtin_*`.
- `crates/kerness/src/assets.rs:79` — `candidates(path, search, builtin)` /
  `:103` `resolve_path(kind, path, search, builtin)` — the ordered places an
  asset path can mean, and the `NotFound` that names every one tried; used by
  [role.md](role.md) and [persona.md](persona.md).
- `crates/kerness/src/assets.rs:139` — `split_frontmatter(text, kind, source)` —
  the line-based splitter for roles, personas, and skills; `:118` `text_field`
  reads one trimmed string out of the mapping.
- `bindings/python/src/funcs.rs:326` — `load_gameplan` / `:334`
  `list_builtin_gameplans` — the pyfunctions; the config crosses as a frozen
  `GameplanConfig` with `harness`, `body`, `raw_text`, `path`, `directory`,
  `requires_orchestrator`, and `max_rounds` getters.

## Interactions

- Produces the `HarnessSpec` that [harness.md](harness.md) parses from the
  frontmatter mapping and later validates against the session's registrations;
  the YAML 1.1 scalar rules are [harness.md](harness.md)'s `yaml.rs`.
- Loaded by [session.md](session.md) at construction
  (`crates/kerness/src/session.rs:550`); `max_rounds()` and
  `requires_orchestrator()` are read there.
- Its `directory()` is the search path for [persona.md](persona.md) and
  [role.md](role.md); `assets::resolve_path` and `assets::split_frontmatter`
  are their loaders' shared body, and [skills.md](skills.md) reads under the
  same root.
- Its built-in list is walked by [selfcheck.md](selfcheck.md), which loads
  every one rather than merely listing them, and by
  `crates/kerness/tests/public_api.rs:178`.
- The assets root is set by [bindings.md](bindings.md)'s `bootstrap`, which is
  the only caller of `set_root`.
- `Error::GameplanLoad` crosses as `GameplanLoadError` through
  [errors.md](errors.md).

## How to Test

```sh
cargo test -p kerness gameplan                                              # pass = 23 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_gameplan_loader.py -q # pass = 13 passed
.venv/bin/python -m pytest bindings/python/tests/test_packaging.py -q       # pass = 4 passed
```

- `crates/kerness/src/gameplan.rs:219` — `the_body_is_the_prose_and_raw_text_is_the_whole_file`,
  `:236` `a_missing_gameplan_is_named_in_the_error`, `:244`
  `every_discovered_gameplan_loads_under_its_own_name`, `:259`
  `debates_bounds_terminators_and_result_shape_are_read`, `:286`
  `discussion_has_no_consensus_terminator`, `:293`
  `every_gameplan_has_think_and_rethink`, `:328`
  `a_custom_gameplan_loads_from_an_absolute_path`, `:342`
  `an_undeclared_name_falls_back_to_the_filename`, `:354`
  `invalid_yaml_reports_the_file`, `:361`
  `a_file_with_no_frontmatter_loads_on_harness_defaults`.
- `crates/kerness/src/assets.rs:178` — `the_repository_copy_is_found_without_any_wiring`,
  `:189` `stems_are_listed_from_disk_and_sorted`, `:197`
  `a_directory_that_is_not_there_lists_as_empty` — the root resolution and
  the enumeration.
- `bindings/python/tests/test_gameplan_loader.py:32` —
  `test_every_discovered_gameplan_loads_under_its_own_name` — the discovery
  assertion from Python: every file on disk is loaded, not merely listed.
- `:28` `test_load_missing_gameplan`, `:115` `test_load_missing_custom_path`,
  `:119` `test_invalid_yaml_reports_the_file`, and `:141`
  `test_a_file_with_no_frontmatter_loads_on_harness_defaults` — a file with no
  contract is valid and takes the defaults; only malformed YAML is an error,
  and it crosses as `GameplanLoadError`.
- `bindings/python/tests/test_packaging.py:42` asserts the crate's assets and
  the package's assets are byte-identical.
- `bindings/python/tests/test_examples.py:148` loads every gameplan shipped
  beside a Python example, so the example harness contract in
  `bindings/python/examples/texas_holdem/gameplan/` is held to the current
  parser.
- Gap: `$KERNESS_ASSETS` has no owning test; the resolution order is proved
  only for the `set_root` and manifest-directory steps.

## Review and Refactor Guide

- **Changing how a name or path resolves** → `resolve_gameplan_path`
  (`crates/kerness/src/gameplan.rs:165`); `a_missing_gameplan_is_named_in_the_error`
  and `test_name_defaults_to_filename`
  (`bindings/python/tests/test_gameplan_loader.py:103`), which loads a
  relative `./name.md`; the session's `gameplan:` string is the public entry.
- **Changing the frontmatter split** → the regex at
  `crates/kerness/src/gameplan.rs:118` and, separately, `assets::split_frontmatter`
  for the other families; a change to one is not a change to the other, and
  both tolerate a file with no frontmatter.
- **Changing `GameplanConfig`'s fields** → `PyGameplanConfig`'s getters
  (`bindings/python/src/types.rs:1852` onward) and `__repr__`; the session
  reads `directory()`, `max_rounds()`, and `requires_orchestrator()`.
- **Adding or removing a built-in gameplan** → add it to both asset copies;
  `bindings/python/tests/test_packaging.py:42`,
  `every_discovered_gameplan_loads_under_its_own_name`,
  `every_gameplan_has_think_and_rethink`, and `selfcheck` each enumerate the
  directory, so nothing else needs listing. The built-ins stay
  framework-generic by project rule ([root deviations](../ARCHITECTURE.md#coding-discipline)).
- **Changing the assets root resolution** → `assets::root`
  (`crates/kerness/src/assets.rs:38`) is read by every loader; `bootstrap`
  is the only `set_root` caller, and `the_repository_copy_is_found_without_any_wiring`
  pins the manifest-directory fallback.
- **Safe extension points**: a new asset family is a new `<family>_dir()`
  over `assets::root()` plus `list_markdown_stems`, `resolve_path`, and
  `split_frontmatter`, following `role.rs` and `persona.rs`.
- **Forbidden coupling**: `gameplan.rs` must not import the session or
  validate against registrations; it parses, and `validate_harness` runs
  later in [session.md](session.md)'s preparation.
- **Compatibility**: `load_gameplan(name_or_path)` and
  `list_builtin_gameplans()` are public on both surfaces; `GameplanConfig`'s
  getter names are the Python contract.

Improvement candidates, as proposals:

- Cover `$KERNESS_ASSETS` with a unit test that sets the variable, clears the
  slot, and asserts `root()`. Success check: the test fails when the
  environment branch at `crates/kerness/src/assets.rs:42` is removed.
- Cache built-in gameplans for callers that enumerate all of them
  (`selfcheck`, `public_api`), since `load_gameplan` reads the file on every
  call. Success check: `list_builtin_gameplans` plus `load_gameplan` over the
  three built-ins reads each file once.

## Open Gaps / Roadmap

- The built-in gameplans stay framework-generic by project rule; a
  domain-specific gameplan belongs with the project that owns the domain, as
  `bindings/python/examples/texas_holdem/gameplan/` demonstrates.
- `load_gameplan` reads the file on every call. Sessions load once, so the
  caching that would help is only for a caller enumerating all built-ins.
- A gameplan cannot include or extend another; the contract is one file.
- Two frontmatter splitters exist — the regex in `gameplan.rs` and the
  line-based one in `assets.rs` — with different tolerance for trailing
  whitespace on the closing delimiter.
