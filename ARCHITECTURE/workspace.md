---
eatmycode_version: "1.2.0"
---

# Workspace map detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#workspace-map): the
full tree with what each path holds, and the edit restrictions and
regeneration steps that follow from it.

## Tree

```
Cargo.toml                  workspace root, shared dependency versions, the one version number
Cargo.lock                  committed; `--locked` commands are a claim about it
crates/
  kerness/                  the framework — pure Rust, links no Python
    src/                    31 top-level modules plus provider/, skill/, and session/;
                            every file opens with a `//!` doc; unit tests inline
    assets/                 built-in gameplans, roles, personas, skills (edit with the copy below)
    tests/                  8 integration files + common/mod.rs doubles, over the public surface
    examples/               10 harnesses driven from Rust alone; support/mod.rs is shared by two
bindings/
  python/                   everything the wheel is built from
    pyproject.toml          the wheel's manifest and the ruff config
    Cargo.toml              the `kerness-py` crate, a workspace member, `publish = false`
    LICENSE  README.md      symlinks to the root copies (load-bearing, see below)
    src/                    13 PyO3 modules, one per boundary concern
    kerness/                the installed Python package
      __init__.py           bootstrap + public surface
      *.py                  per-subsystem re-export shims
      provider.py  channel.py  memory.py     the three subclassable ABCs
      access.py             the AccessPolicy dataclass
      exceptions.py         the exception hierarchy
      _enums.py             ToolDialect
      selfcheck.py          `python -m kerness.selfcheck`
      _core.abi3.so         BUILT ARTIFACT, gitignored; regenerate with `maturin develop`
      gameplans/ roles/ personas/ skills/   byte-identical copies of crates/kerness/assets/
    tests/                  26 pytest modules + conftest.py
    examples/               8 runnable scripts, walked recursively by tests/test_examples.py
.github/workflows/          ci.yml (every push and PR); release.yml (wheels, sdist, clean sdist install)
.github/dependabot.yml      weekly cargo and github-actions bumps
assets/                     project marks only: logo.svg, logo-mark.svg
README.md                   the public introduction
ARCHITECTURE.md             root rules; AGENTS.md, AGENT.md and CLAUDE.md alias it
ARCHITECTURE/               one owner file per subsystem plus the supporting pages index.md lists
.venv/                      local virtualenv, gitignored; `.venv/bin/{python,maturin,ruff,pytest}`
target/  .ruff_cache/  .pytest_cache/  __pycache__/   generated, gitignored
```

## Edit restrictions and regeneration

- **Assets are declared twice.** Change `crates/kerness/assets/<kind>/<file>`
  and `bindings/python/kerness/<kind>/<file>` together;
  `bindings/python/tests/test_packaging.py:42` fails otherwise.
- **The two symlinks in `bindings/python/` are load-bearing.** `readme` and
  `license-files` in `pyproject.toml` resolve against that directory and
  reject a `..` path; without them the wheel builds and ships neither, with
  nothing on stderr to say so.
- **`_core.abi3.so`** is what `maturin develop` writes into the package
  directory. After any Rust change, rebuild before running the Python suite;
  `bindings/python/tests/test_packaging.py:35` detects a version mismatch only,
  not source changes within the same version.
- **The wheel carries the package and distribution metadata**, including license
  and README metadata from `pyproject.toml`; examples and tests are outside the
  installed package. The sdist is
  rooted at the workspace and carries `crates/` as well.
- **No generated source.** There are no build scripts, no codegen, and no
  `.pyi` stubs.
