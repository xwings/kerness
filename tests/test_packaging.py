"""Packaging invariants.

The version is declared twice — once for pip and once for callers reading
``kerness.__version__`` — and nothing but this test keeps the two in step.
"""

from pathlib import Path

import kerness

ROOT = Path(__file__).resolve().parent.parent


def _declared_version() -> str:
    for line in (ROOT / "pyproject.toml").read_text().splitlines():
        if line.startswith("version = "):
            return line.split("=", 1)[1].strip().strip('"')
    raise AssertionError("pyproject.toml declares no version")


def test_the_package_and_pyproject_agree_on_the_version():
    assert kerness.__version__ == _declared_version()
