"""Packaging invariants.

Two things in this repository are declared twice because two distributions
need them, and nothing but this file keeps either pair in step: the version,
once for pip and once for callers reading ``kerness.__version__``; and the
built-in gameplans, personas, and skills, once inside the Rust crate and once
inside the Python package.
"""

from pathlib import Path

import kerness

ROOT = Path(__file__).resolve().parent.parent
CRATE_ASSETS = ROOT / "crates" / "kerness" / "assets"
PACKAGE_ASSETS = Path(kerness.__file__).resolve().parent


def _declared_version() -> str:
    for line in (ROOT / "pyproject.toml").read_text().splitlines():
        if line.startswith("version = "):
            return line.split("=", 1)[1].strip().strip('"')
    raise AssertionError("pyproject.toml declares no version")


def test_the_package_and_pyproject_agree_on_the_version():
    assert kerness.__version__ == _declared_version()


def test_the_crate_and_the_package_ship_the_same_assets():
    """A Rust caller and a Python caller who both load ``debate`` have to get
    the same gameplan. The crate cannot read the package's copy — it is not
    there when the crate is used alone — so the file exists twice, and a fix
    applied to one copy silently leaves the other wrong."""
    for kind in ("gameplans", "personas", "skills"):
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
