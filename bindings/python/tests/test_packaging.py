"""Packaging invariants.

The version is declared once, as ``[workspace.package] version`` in the root
``Cargo.toml``. Everything else derives from it: ``pyproject.toml`` is
``dynamic``, and ``kerness.__version__`` is what the extension was compiled
with. That chain runs through maturin and a rebuild, so this file checks the
number that arrives rather than the one that was written.

The built-in gameplans, personas, and skills are the one thing still declared
twice, because two distributions need them: once inside the Rust crate and once
inside the Python package. Nothing in the build keeps that pair in step.
"""

import importlib
import pkgutil
from pathlib import Path

import kerness

ROOT = Path(__file__).resolve().parents[3]
CRATE_ASSETS = ROOT / "crates" / "kerness" / "assets"
PACKAGE_ASSETS = Path(kerness.__file__).resolve().parent


def _workspace_version() -> str:
    section = ""
    for line in (ROOT / "Cargo.toml").read_text().splitlines():
        if line.startswith("["):
            section = line.strip()
        elif section == "[workspace.package]" and line.startswith("version = "):
            return line.split("=", 1)[1].strip().strip('"')
    raise AssertionError("Cargo.toml declares no [workspace.package] version")


def test_the_package_reports_the_workspace_version():
    """A stale extension is the failure this catches: the wheel is named for the
    workspace version, so an installed package reporting an older one means the
    binary in ``site-packages`` predates the bump it is being tested against."""
    assert kerness.__version__ == _workspace_version()


def test_the_crate_and_the_package_ship_the_same_assets():
    """A Rust caller and a Python caller who both load ``debate`` have to get
    the same gameplan. The crate cannot read the package's copy — it is not
    there when the crate is used alone — so the file exists twice, and a fix
    applied to one copy silently leaves the other wrong."""
    for kind in ("gameplans", "roles", "personas", "skills"):
        crate = {
            path.relative_to(CRATE_ASSETS): path.read_text(encoding="utf-8")
            for path in sorted((CRATE_ASSETS / kind).rglob("*.md"))
        }
        package = {
            path.relative_to(PACKAGE_ASSETS): path.read_text(encoding="utf-8")
            for path in sorted((PACKAGE_ASSETS / kind).rglob("*.md"))
        }
        assert crate, f"the crate ships no {kind}"
        assert crate == package


def _public_modules() -> list[str]:
    """Every importable module in the package that is not private.

    Enumerated rather than listed, so a module added later is held to the same
    rule without anyone remembering to name it here. ``selfcheck`` is excluded
    because it is a ``__main__`` script, not a surface.
    """
    return sorted(
        module.name
        for module in pkgutil.iter_modules(kerness.__path__)
        if not module.name.startswith("_") and module.name != "selfcheck"
    )


def test_every_public_module_declares_what_it_exports():
    """``import *`` is the reader's question "what is in here?", and a module
    with no ``__all__`` answers it with whatever it happened to import — so
    ``from kerness.channel import *`` would hand back ``Path`` and ``json``."""
    missing = [name for name in _public_modules()
               if not hasattr(importlib.import_module(f"kerness.{name}"), "__all__")]
    assert not missing, f"public modules with no __all__: {missing}"


def test_every_exported_name_resolves():
    """A name in ``__all__`` that the module does not define is an
    ``AttributeError`` on ``import *`` and nothing at all until someone tries
    it — which for a re-export shim means a renamed symbol on the Rust side."""
    broken = {}
    for name in ["__init__", *_public_modules()]:
        module = kerness if name == "__init__" else importlib.import_module(f"kerness.{name}")
        absent = [export for export in module.__all__ if not hasattr(module, export)]
        if absent:
            broken[name] = absent
    assert not broken, f"exported names that do not resolve: {broken}"
