"""Tests for the runnable scripts in examples/.

The examples cannot be executed here — they need API keys and network. What
can be checked is the part that actually rots: every name they reach for must
still exist on the package, and every gameplan they ship must still load.

This is not hypothetical. A refactor that renames a public method leaves the
examples syntactically valid and silently wrong; the failure only appears for
a user with credentials, which is the worst place to discover it.
"""

import ast
import os
from pathlib import Path

import pytest

import kerness
from kerness.gameplan_loader import load_gameplan

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXAMPLES = os.path.join(REPO, "examples")


def _scripts():
    found = []
    for root, _dirs, files in os.walk(EXAMPLES):
        found += [
            os.path.join(root, f) for f in files if f.endswith(".py")
        ]
    return sorted(found)


def _rel(path):
    return os.path.relpath(path, REPO)


def _callee(func):
    """Dotted name of a call target: `Session` or `kerness.Session`."""
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute) and isinstance(func.value, ast.Name):
        return f"{func.value.id}.{func.attr}"
    return ""


def _missing_module_attributes(tree):
    """Every `kerness.X` the script names, and every from-import it makes."""
    missing = [
        node.attr
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.value, ast.Name)
        and node.value.id == "kerness"
        and not hasattr(kerness, node.attr)
    ]
    for node in ast.walk(tree):
        if not isinstance(node, ast.ImportFrom):
            continue
        if not (node.module or "").startswith("kerness"):
            continue
        mod = __import__(node.module, fromlist=["_"])
        missing += [
            f"{node.module}.{a.name}"
            for a in node.names
            if not hasattr(mod, a.name)
        ]
    return sorted(set(missing))


def _missing_session_methods(tree):
    """`kerness.Session` surviving a rename says nothing about
    `session.run()` surviving one, and a renamed method leaves the example
    parsing cleanly."""
    # Names bound to a Session(...) call — usually `session`, but the examples
    # are not required to agree on that.
    sessions = {
        t.id
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        and isinstance(node.value, ast.Call)
        and _callee(node.value.func) in ("Session", "kerness.Session")
        for t in node.targets
        if isinstance(t, ast.Name)
    }
    called = {
        node.func.attr
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id in sessions
    }
    return sorted(m for m in called if not hasattr(kerness.Session, m))


def _missing_result_attributes(tree):
    """An example can parse cleanly, call only live Session methods, and
    still raise `AttributeError` on the line after `run()` because it reads a
    result attribute that does not exist."""
    results = {
        t.id
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Attribute)
        and node.value.func.attr == "run"
        for t in node.targets
        if isinstance(t, ast.Name)
    }
    read = {
        node.attr
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.value, ast.Name)
        and node.value.id in results
    }
    # Checked against an instance, not the class: a removed alias kept as a
    # raising property still answers `hasattr` on the class.
    probe = kerness.SessionResult(
        topic="", turns_completed=0, consensus_reached=False
    )
    return sorted(a for a in read if not hasattr(probe, a))


class TestPublicSurface:
    def test_there_are_examples_to_check(self):
        """Guards against the walk silently finding nothing."""
        assert len(_scripts()) >= 6

    @pytest.mark.parametrize("path", _scripts(), ids=_rel)
    def test_every_name_it_reaches_for_still_exists(self, path):
        """Parsing is the cheap half; the module attributes, Session methods,
        and result attributes are the halves that actually rot. All four are
        collected before asserting so one stale name does not hide the rest."""
        tree = ast.parse(Path(path).read_text(encoding="utf-8"), filename=path)

        stale = {
            "kerness attributes": _missing_module_attributes(tree),
            "Session methods": _missing_session_methods(tree),
            "SessionResult attributes": _missing_result_attributes(tree),
        }
        broken = {kind: names for kind, names in stale.items() if names}
        assert not broken, f"{_rel(path)} names what no longer exists: {broken}"


class TestExampleGameplans:
    def test_every_shipped_gameplan_loads_under_the_current_schema(self):
        """A gameplan whose contract the current schema rejects raises, not
        warns — and a walk that found nothing would report that as success."""
        found = sorted(
            os.path.join(root, f)
            for root, _dirs, files in os.walk(EXAMPLES)
            for f in files
            if f.endswith(".md") and "gameplan" in root
        )
        assert found, "no example gameplans found at all"

        for path in found:
            config = load_gameplan(path)
            assert config.harness.loop.terminate_on, f"{path}: no terminate_on"
