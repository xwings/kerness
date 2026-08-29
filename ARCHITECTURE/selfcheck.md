# Self-Check

## Goal

`python3 -m kerness.selfcheck` answers one question: is this installation
usable, and which optional features are available? It imports every core module,
loads every built-in asset, reports optional dependencies, and exits 0 or 1.

It is the first thing to run against a fresh wheel, because the failure it is
designed to catch — an extension that built but cannot import, or assets that
shipped but do not parse — produces no error until something tries.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `bindings/python/kerness/selfcheck.py` | the whole check |

Deliberately Python, not a Rust entry point: the failure it exists to catch is a
broken *Python* import, which a Rust binary cannot observe. Tests also
monkeypatch both module lists, which requires them to be module attributes.

## Key Types and Entry Points

- `bindings/python/kerness/selfcheck.py:15` — `_CORE_MODULES` — 25 `(module, label)` pairs
  that must import; the list is the definition of "core".
- `bindings/python/kerness/selfcheck.py:46` — `_OPTIONAL` — `(import, label, what it enables)`;
  absence is reported, never fatal.
- `bindings/python/kerness/selfcheck.py:51` — `_check_imports(failures)` — catches anything,
  including a non-`ImportError` raised at import time.
- `bindings/python/kerness/selfcheck.py:62` — `_check_assets(failures)` — enumerates each
  built-in and *loads* it. A gameplan that lists but declares no `terminate_on`
  fails here (`:74`), which is the project's "assert discovery, not literals" rule
  in force.
- `bindings/python/kerness/selfcheck.py:126` — `main()` — returns the exit code; `:143`
  prints `OK: all core checks passed` on success.

The comment at `:38` records the one naming constraint in the package:
`kerness.skills` is the SKILL.md data directory, so the runtime module is
`skill_runtime` to avoid shadowing it.

## Interactions

- Imports every module in the package, so it transitively touches every subsystem
  doc in the Index.
- Loads assets through [gameplan.md](gameplan.md), [role.md](role.md),
  [skills.md](skills.md), and [persona.md](persona.md).
- Its `_CORE_MODULES` list and `bindings/python/kerness/*.py` must stay in step; a new
  subsystem shim that is not listed is not checked.

## How to Test

```sh
.venv/bin/python -m kerness.selfcheck                                 # pass = exit 0
.venv/bin/python -m pytest bindings/python/tests/test_selfcheck.py -q # pass = 0 failed
```

- `.venv/bin/python -m kerness.selfcheck` — pass = output ends with
  `OK: all core checks passed` and exit code 0.
- `bindings/python/tests/test_selfcheck.py:18` — `test_every_package_module_is_in_the_core_list` —
  walks `bindings/python/kerness/` and asserts every module appears in `_CORE_MODULES`, so
  a new shim that is not listed fails the suite.
- `bindings/python/tests/test_selfcheck.py:36` — `test_every_asset_class_is_enumerated_from_disk` —
  the "assert discovery, not literals" rule, tested directly.
- `:100` and `:109` monkeypatch a broken core module and a broken asset to prove
  each produces exit code 1.

## Open Gaps / Roadmap

- `_CORE_MODULES` is a literal list, unlike the asset checks, so a new shim must
  be added by hand. The omission is caught by
  `bindings/python/tests/test_selfcheck.py:18` rather than by the check itself — which means it is
  caught in this repository but not in an installed wheel.
- The check imports and loads but does not run anything — a session that would
  fail on its first provider call still reports a healthy installation.
