---
eatmycode_version: "1.2.0"
---

# Self-Check

## Goal

`python3 -m kerness.selfcheck` answers one question: is this installation
usable, and which optional features are available? It imports every core
module, loads every built-in asset, reports optional dependencies, and exits 0
or 1.

It is the first thing to run against a fresh wheel, because the failure it is
designed to catch — an extension that built but cannot import, or assets that
shipped but do not parse — produces no error until something tries. It owns the
check and its exit code only; the loaders it calls belong to
[gameplan.md](gameplan.md), [role.md](role.md), [persona.md](persona.md) and
[skills.md](skills.md), and it runs no session.

## Status

`done` — `.venv/bin/python -m kerness.selfcheck` prints `OK: all core checks
passed` and exits 0; `bindings/python/tests/test_selfcheck.py` passes 6 tests.
CI runs the check against the developed extension
(`.github/workflows/ci.yml:110`) and against a source distribution installed
into a clean interpreter (`.github/workflows/release.yml:110`).

## Code Structure

| File | Role |
| ---- | ---- |
| `bindings/python/kerness/selfcheck.py` | the whole check: the core-module list, the optional list, the three check functions and `main` |
| `bindings/python/tests/test_selfcheck.py` | coverage and exit-code tests |

Deliberately Python, not a Rust entry point: the failure it exists to catch is
a broken *Python* import, which a Rust binary cannot observe. Tests also
monkeypatch both module lists, which requires them to be module attributes.

## Language and Conventions

Python only, one module in the installed package; the root's [Coding Style
and Code Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply
and `ruff check bindings/python` (rules `E4`, `E7`, `E9`, `F`) enforces them.
Local facts:

- It is the one package module that is a `__main__` script rather than a
  re-export shim, so `bindings/python/tests/test_packaging.py:60` excludes it
  from the `__all__` rule every other public module is held to.
- Every check catches bare `Exception` under `# noqa: BLE001`
  (`bindings/python/kerness/selfcheck.py:55`, `:76`, `:87`, `:100`, `:111`):
  the point is to report anything at all, including a non-`ImportError`
  raised at import time. The `BLE` rule set is not selected in
  `bindings/python/pyproject.toml`, so the markers are documentation rather
  than a suppression the linter needs.
- Output is `print` to stdout, which is what `capsys` reads in
  `bindings/python/tests/test_selfcheck.py:61` and `:109`; this is the
  package's only module that prints by design.
- Tests follow the package convention: `Test<Behaviour>` classes and
  `test_<sentence>` functions, with `monkeypatch` on module attributes.

## Design and Invariants

Three passes, each appending a label to one `failures` list, then one exit
code. `_check_imports` (`bindings/python/kerness/selfcheck.py:51`) imports each
`(module, label)` in `_CORE_MODULES`. `_check_assets` (`:62`) enumerates every
built-in gameplan, skill, role and persona through its `list_builtin_*`
function and *loads* each one, refusing a gameplan with no `terminate_on`
(`:74`) and a role with no body (`:98`). `_check_optional` (`:116`) reports
`_OPTIONAL` imports as `PASS` or `SKIP` and never touches `failures`.

Invariants a change must preserve:

- **Assets are discovered, not listed.** The asset pass calls the loaders'
  enumeration functions — `crates/kerness/src/gameplan.rs:109`,
  `crates/kerness/src/skill/loader.rs:113`, `crates/kerness/src/role.rs:123`,
  `crates/kerness/src/persona.rs:82` — so an added or removed asset cannot
  escape it. Enforced by `test_every_asset_class_is_enumerated_from_disk`
  (`bindings/python/tests/test_selfcheck.py:36`) and
  `test_the_check_reports_every_asset_on_disk` (`:61`), which pins what the
  check prints rather than only which helper it calls.
- **Core modules are a literal list held in step by a test.** `_CORE_MODULES`
  (`bindings/python/kerness/selfcheck.py:15`) is the definition of "core"; `test_every_package_module_is_in_the_core_list`
  (`bindings/python/tests/test_selfcheck.py:18`) walks the package directory
  and fails on any unlisted module. The comment at
  `bindings/python/kerness/selfcheck.py:38` records the one naming constraint in the package: `kerness.skills` is the `SKILL.md` data
  directory, so the runtime module is `skill_runtime` to avoid shadowing it.
- **Optional absence is a SKIP, never a failure.** Enforced by
  `test_a_healthy_install_exits_zero_even_without_the_extras` (`:88`).
- **A failure of any kind is exit 1 and names its label.** `main` (`bindings/python/kerness/selfcheck.py:126`)
  returns 1 when `failures` is non-empty and prints `OK: all core checks
  passed` (`:143`) otherwise. Enforced by `test_a_broken_core_module_exits_nonzero`
  (`bindings/python/tests/test_selfcheck.py:100`) and
  `test_a_broken_asset_exits_nonzero` (`:109`).
- **Nothing here calls a provider or starts a session.** Observed; no test
  enforces it.

The Rust side proves the same asset properties for a crate-only caller in
`crates/kerness/tests/public_api.rs:178`, `:197` and `:219`; the two are the
two halves of one guarantee and neither substitutes for the other.

## Key Types and Entry Points

- `bindings/python/kerness/selfcheck.py:15` — `_CORE_MODULES` — 25
  `(module, label)` pairs that must import.
- `bindings/python/kerness/selfcheck.py:46` — `_OPTIONAL` —
  `(import, label, what it enables)`; absence is reported, never fatal.
- `bindings/python/kerness/selfcheck.py:51` — `_check_imports(failures)` —
  imports each core module, prints `PASS`/`FAIL`, appends failed labels.
- `bindings/python/kerness/selfcheck.py:62` — `_check_assets(failures)` —
  enumerates each built-in class from disk and loads every member.
- `bindings/python/kerness/selfcheck.py:116` — `_check_optional()` — prints
  `PASS` or `SKIP` per optional import; no side effect on the exit code.
- `bindings/python/kerness/selfcheck.py:126` — `main()` — runs the three
  passes in order and returns the process exit code.

## Interactions

- Imports every module in the package, so it transitively touches every
  subsystem doc in the Index; the extension is reached first as
  `kerness._core`.
- Loads assets through [gameplan.md](gameplan.md)'s `load_gameplan`,
  [role.md](role.md)'s `load_role`, [skills.md](skills.md)'s `load_skill` and
  [persona.md](persona.md)'s `load_persona`, and enumerates them through the
  matching `list_builtin_*` functions; the asset root is the one
  [bindings.md](bindings.md)'s `bootstrap` installed at import.
- Its `_CORE_MODULES` list and `bindings/python/kerness/*.py` must stay in
  step; `bindings/python/tests/test_selfcheck.py:18` is where that is tested.
- [testing.md](testing.md) runs it in CI after `maturin develop` and after a
  clean sdist install; the second run is what proves the installed package
  rather than the working tree.

## How to Test

```sh
.venv/bin/python -m kerness.selfcheck                                 # pass = last line "OK: all core checks passed", exit 0
.venv/bin/python -m pytest bindings/python/tests/test_selfcheck.py -q # pass = 6 passed
```

- `bindings/python/tests/test_selfcheck.py:18` —
  `test_every_package_module_is_in_the_core_list` — walks
  `bindings/python/kerness/` and asserts every module appears in
  `_CORE_MODULES`, so a new shim that is not listed fails the suite.
- `bindings/python/tests/test_selfcheck.py:36` —
  `test_every_asset_class_is_enumerated_from_disk` — the "assert discovery,
  not literals" rule, tested directly.
- `bindings/python/tests/test_selfcheck.py:61` —
  `test_the_check_reports_every_asset_on_disk` — every enumerated asset name
  appears in what the check prints.
- `bindings/python/tests/test_selfcheck.py:88`, `:100` and `:109` — a
  missing optional is exit 0; a broken core module and a broken asset are
  each exit 1.
- Gap: no test asserts that a role with an empty body (`selfcheck.py:98`)
  fails the roles pass; the Rust counterpart
  `every_bundled_role_loads_and_carries_a_prompt`
  (`crates/kerness/tests/public_api.rs:197`) covers the property for the
  crate's copy only.

## Review and Refactor Guide

- Adding a package module → add its `(module, label)` pair to
  `_CORE_MODULES` (`bindings/python/kerness/selfcheck.py:15`);
  `test_every_package_module_is_in_the_core_list` fails until you do. If it is
  a new kind of module, add it to [bindings.md](bindings.md)'s Code Structure.
- Adding an asset class → add a fourth block to `_check_assets` (`:62`)
  following the existing shape (enumerate, load each, print `PASS <class>
  (<names>)`), extend `test_every_asset_class_is_enumerated_from_disk` and
  `test_the_check_reports_every_asset_on_disk`, and add the class to the Rust
  side in `crates/kerness/tests/public_api.rs`.
- Adding an optional dependency → append to `_OPTIONAL` (`:46`); the
  `enables` text is what a user reads on `SKIP`, so name the feature it gates.
- Changing the output format → `test_the_check_reports_every_asset_on_disk`
  and `test_a_broken_asset_exits_nonzero` match printed text; the `OK:` line
  at `:143` is the evidence string every How to Test in this doc set cites.
- Forbidden coupling: no import of a provider backend at module scope, no
  network, no session construction; the check must stay runnable with no key
  and no `pydantic`.
- Compatibility check: `bindings/python/tests/test_packaging.py:60` excludes
  `selfcheck` from the `__all__` rule by name; renaming the module breaks that
  exclusion.

Improvement candidates (proposals, not accepted work):

- Derive `_CORE_MODULES` from `pkgutil.iter_modules` the way
  `bindings/python/tests/test_packaging.py:60` already does, keeping only the label map literal.
  Benefit: an unlisted shim is caught in an installed wheel, not only in this
  repository. Check: `test_every_package_module_is_in_the_core_list` becomes
  redundant and is retired with a citation to the derivation.
- Assert the empty-role-body refusal from Python. Benefit: closes the gap
  named above. Check: one monkeypatched case in `TestExitCode` returning a
  `RoleConfig` with empty `content` and asserting `"roles" in failures`.

## Open Gaps / Roadmap

- `_CORE_MODULES` is a literal list, unlike the asset checks, so a new shim
  must be added by hand. The omission is caught by
  `bindings/python/tests/test_selfcheck.py:18` rather than by the check
  itself — which means it is caught in this repository but not in an
  installed wheel.
- The check imports and loads but does not run anything — a session that
  would fail on its first provider call still reports a healthy installation.
