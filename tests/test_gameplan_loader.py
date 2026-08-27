"""Tests for kerness.gameplan_loader."""

import os
import tempfile

import pytest

from kerness.exceptions import GameplanLoadError
from kerness.gameplan_loader import list_builtin_gameplans, load_gameplan


class TestLoadGameplan:
    def test_the_body_is_the_prose_and_raw_text_is_the_whole_file(self):
        """The body is what the orchestrator reads; YAML must not leak in."""
        config = load_gameplan("debate")

        assert config.name == "debate"
        assert config.requires_orchestrator is True
        assert config.max_rounds == 3

        assert "# Debate" in config.body
        assert "---" not in config.body.split("\n")[0]
        assert "terminate_on" not in config.body

        assert config.raw_text.startswith("---")
        assert "name: debate" in config.raw_text

    def test_load_missing_gameplan(self):
        with pytest.raises(GameplanLoadError, match="not found"):
            load_gameplan("nonexistent_gameplan")

    def test_every_discovered_gameplan_loads_under_its_own_name(self):
        """Enumerated rather than listed, so a fourth bundled gameplan cannot
        ship without this check applying to it."""
        names = list_builtin_gameplans()
        assert {"debate", "discussion", "research"} <= set(names)

        for name in names:
            config = load_gameplan(name)
            assert config.name == name
            assert config.requires_orchestrator is True


class TestHarnessFromFrontmatter:
    """The gameplan defines the harness — these assert it actually does."""

    def test_debates_bounds_terminators_and_result_shape_are_read(self):
        harness = load_gameplan("debate").harness

        assert harness.agents.participants.min == 2
        assert harness.agents.participants.max == 6

        assert harness.loop.terminate_on == ("END_SESSION", "CONSENSUS_REACHED")
        assert harness.loop.consensus_keyword == "CONSENSUS_REACHED"

        fields = {f.name: f for f in harness.result}
        assert fields["consensus"].py_type is bool
        assert fields["summary"].py_type is str

    def test_discussion_has_no_consensus_terminator(self):
        harness = load_gameplan("discussion").harness
        assert harness.loop.terminate_on == ("END_SESSION",)
        assert harness.loop.consensus_keyword is None

    @pytest.mark.parametrize("name", list_builtin_gameplans())
    def test_every_gameplan_has_think_and_rethink(self, name):
        """The think/rethink principle is structural, not prose."""
        phases = load_gameplan(name).harness.loop.phases
        assert phases, f"{name} declares no phases"
        assert phases[0].name == "think"
        assert phases[0].rethink is False
        rethinks = [p for p in phases if p.rethink]
        assert len(rethinks) == 1, f"{name} must have exactly one rethink phase"
        assert rethinks[0] is phases[-1], "rethink must come last"

        for phase in phases:
            assert phase.instruction, f"{name}.{phase.name} has no instruction"


class TestCustomGameplans:
    def test_load_from_absolute_path(self):
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write(
                "---\n"
                "name: custom\n"
                "agents:\n"
                "  orchestrator: false\n"
                "loop:\n"
                "  max_rounds: 7\n"
                "---\n\n"
                "# Custom\n"
            )
            f.flush()
            config = load_gameplan(f.name)
            assert config.name == "custom"
            assert config.requires_orchestrator is False
            assert config.max_rounds == 7
            assert config.body.strip() == "# Custom"
        os.unlink(f.name)

    def test_name_defaults_to_filename(self):
        """A temp filename with underscores must not fail slug validation."""
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False, dir="."
        ) as f:
            f.write("---\nloop:\n  max_rounds: 2\n---\n\n# Relative\n")
            f.flush()
            basename = os.path.basename(f.name)
            config = load_gameplan(f"./{basename}")
            assert config.name == os.path.splitext(basename)[0]
        os.unlink(f.name)

    def test_load_missing_custom_path(self):
        with pytest.raises(GameplanLoadError, match="not found"):
            load_gameplan("/nonexistent/custom_gameplan.md")

    def test_invalid_yaml_reports_the_file(self):
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write("---\nname: [unclosed\n---\n\n# Bad\n")
            f.flush()
            with pytest.raises(GameplanLoadError, match="Invalid YAML"):
                load_gameplan(f.name)
        os.unlink(f.name)


class TestPlainMarkdown:
    """A gameplan does not have to declare a contract."""

    def _write(self, body: str) -> str:
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write(body)
            f.flush()
            return f.name

    def test_a_file_with_no_frontmatter_loads_on_harness_defaults(self):
        """The whole file becomes the instruction body, quietly. This is what
        keeps a one-paragraph custom gameplan viable."""
        path = self._write("# Plain\n\nJust instructions, no contract.\n")
        config = load_gameplan(path)
        assert config.harness.loop.terminate_on == ("END_SESSION",)
        assert "Just instructions" in config.body
        os.unlink(path)
