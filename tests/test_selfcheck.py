"""Tests for kerness.selfcheck.

The self-check is the one thing that must work when nothing else does, so it
is tested for the property that matters: it fails loudly rather than passing
vacuously. A check that silently skips what it cannot find is worse than no
check at all — the whole reason this module exists is that a broken import
once went unnoticed.
"""

import kerness.selfcheck as selfcheck
from kerness.gameplan_loader import list_builtin_gameplans
from kerness.skill_loader import list_builtin_skills


class TestCoverage:
    """What the check actually covers, asserted rather than assumed."""

    def test_every_package_module_is_in_the_core_list(self):
        """A new module that nobody adds here is a module whose import
        failure the self-check would miss."""
        import os

        import kerness

        package_dir = os.path.dirname(kerness.__file__)
        on_disk = {
            f[:-3]
            for f in os.listdir(package_dir)
            if f.endswith(".py") and not f.startswith("_")
        }
        # A running module cannot usefully test-import itself.
        on_disk.remove("selfcheck")
        listed = {m.split(".")[-1] for m, _ in selfcheck._CORE_MODULES}
        assert on_disk <= listed, f"not covered by selfcheck: {on_disk - listed}"

    def test_every_asset_class_is_enumerated_from_disk(self):
        """The asset list is enumerated from disk, not hardcoded — that is the
        whole point, so assert the property rather than a name. Naming one here
        would recreate the bug: a list that goes stale when the set changes.
        Hardcoding meant the fourth gameplan was never loaded, and personas
        were the last class still hardcoded — the same defect surviving in the
        one place nobody looked."""
        from pathlib import Path

        import kerness
        from kerness.persona_loader import list_builtin_personas

        root = Path(kerness.__file__).parent
        for subdir, listing in (
            ("gameplans", list_builtin_gameplans()),
            ("personas", list_builtin_personas()),
        ):
            on_disk = {p.stem for p in (root / subdir).glob("*.md")}
            assert on_disk, f"no {subdir} found at all"
            assert listing == sorted(on_disk)

        assert {p.parent.name for p in root.glob("skills/*/SKILL.md")} == set(
            list_builtin_skills()
        )

    def test_the_check_reports_every_asset_on_disk(self, capsys):
        """Pinning the enumeration helpers is not enough: ``_check_assets``
        could call one and print a literal, and every other test here would
        still pass. This asserts what the check actually says.

        Verified by replacing each of the three ``list_builtin_*`` calls in
        ``selfcheck.py`` with a one-item literal — each substitution fails
        here, and failed nowhere else.
        """
        failures: list[str] = []
        selfcheck._check_assets(failures)
        printed = capsys.readouterr().out

        from kerness.persona_loader import list_builtin_personas

        expected = (
            list_builtin_gameplans()
            + list_builtin_skills()
            + list_builtin_personas()
        )
        assert expected, "no built-in assets found at all"
        missing = [name for name in expected if name not in printed]
        assert not missing, f"enumerated but never reported: {missing}"
        assert failures == []


class TestExitCode:
    def test_a_healthy_install_exits_zero_even_without_the_extras(
        self, monkeypatch
    ):
        """pydantic being absent is a SKIP, not a failure — that distinction
        is the entire point of an optional dependency."""
        assert selfcheck.main() == 0

        monkeypatch.setattr(
            selfcheck, "_OPTIONAL", [("no_such_module", "phantom", "nothing")]
        )
        assert selfcheck.main() == 0

    def test_a_broken_core_module_exits_nonzero(self, monkeypatch):
        """The failure mode this module was written for."""
        monkeypatch.setattr(
            selfcheck,
            "_CORE_MODULES",
            [("kerness.does_not_exist", "phantom")],
        )
        assert selfcheck.main() == 1

    def test_a_broken_asset_exits_nonzero(self, monkeypatch, capsys):
        """Assets must load, not merely exist."""

        def boom(name):
            raise ValueError("corrupt")

        monkeypatch.setattr(
            "kerness.gameplan_loader.load_gameplan", boom
        )
        failures = []
        selfcheck._check_assets(failures)
        assert "gameplans" in failures
        assert "FAIL gameplans" in capsys.readouterr().out
