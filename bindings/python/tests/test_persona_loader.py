"""Tests for kerness.persona_loader."""

import os
import tempfile
from pathlib import Path

import pytest

from kerness.persona_loader import (
    PersonaConfig,
    format_persona_for_prompt,
    list_builtin_personas,
    load_persona,
    resolve_persona_path,
)


class TestLoadPersona:
    def test_every_section_is_read_and_a_missing_one_is_empty(self):
        """An absent section must read as empty rather than carry the next
        section's prose, which is what a greedy split would do."""
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write(
                "# Persona: Test Agent\n\n"
                "## Persona\nA helpful test agent.\n\n"
                "## Background\n10 years of testing experience.\n\n"
                "## Communication Style\nPrecise and methodical.\n"
            )
            f.flush()
            config = load_persona(f.name)
            assert config.name == "Test Agent"
            assert config.persona == "A helpful test agent."
            assert config.background == "10 years of testing experience."
            assert config.communication_style == "Precise and methodical."
        os.unlink(f.name)

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write("# Persona: Minimal\n\n## Persona\nJust a persona.\n")
            f.flush()
            config = load_persona(f.name)
            assert config.name == "Minimal"
            assert config.persona == "Just a persona."
            assert config.background == ""
            assert config.communication_style == ""
        os.unlink(f.name)

    def test_a_relative_path_resolves_and_a_missing_one_says_so(self):
        """A path the caller typed is relative to where they typed it."""
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False, dir="."
        ) as f:
            f.write("# Persona: Relative\n\n## Persona\nA relative persona.\n")
            f.flush()
            config = load_persona(f"./{os.path.basename(f.name)}")
            assert config.name == "Relative"
            assert config.persona == "A relative persona."
        os.unlink(f.name)

        with pytest.raises(FileNotFoundError, match="not found"):
            load_persona("/nonexistent/path/persona.md")


class TestFormatPersonaForPrompt:
    def test_only_the_sections_that_have_content_are_rendered(self):
        """An empty section rendered anyway would put a bare 'Background:' in
        the system prompt, and a persona with nothing in it would leave a
        heading with no body under it."""
        full = format_persona_for_prompt(PersonaConfig(
            name="Test",
            persona="A tester",
            background="Testing background",
            communication_style="Direct",
        ))
        assert "Persona: A tester" in full
        assert "Background: Testing background" in full
        assert "Communication style: Direct" in full

        partial = format_persona_for_prompt(
            PersonaConfig(name="Partial", persona="A persona")
        )
        assert "Persona: A persona" in partial
        assert "Background" not in partial

        assert format_persona_for_prompt(PersonaConfig(name="Empty")) == ""


PERSONA_TEXT = "# Persona: Sib\n\n## Persona\nA sibling persona.\n"


class TestTheSearchPath:
    """The session owns a search path an Agent cannot see. The gameplan's own
    directory is on it so a third-party project can ship a gameplan and its
    personas together and have the paths inside that gameplan mean what they
    say from any working directory."""

    def test_a_bare_name_finds_the_builtin(self):
        assert resolve_persona_path("pragmatic_engineer.md").exists()

    def test_a_search_directory_is_tried_by_resolve_and_by_load(self, tmp_path):
        """``load_persona`` taking a different path from ``resolve_persona_path``
        would mean a persona the session can find but not read."""
        (tmp_path / "sib.md").write_text(PERSONA_TEXT, encoding="utf-8")

        assert resolve_persona_path("sib.md", search=[tmp_path]) == tmp_path / "sib.md"
        assert load_persona("sib.md", search=[tmp_path]).persona == "A sibling persona."

    def test_the_working_directory_wins_over_the_search_path(
        self, tmp_path, monkeypatch
    ):
        """A path the caller can see resolved from where they typed it beats
        one the framework supplied on their behalf."""
        near = tmp_path / "near"
        far = tmp_path / "far"
        near.mkdir()
        far.mkdir()
        (near / "sib.md").write_text(PERSONA_TEXT, encoding="utf-8")
        (far / "sib.md").write_text(PERSONA_TEXT, encoding="utf-8")
        monkeypatch.chdir(near)

        assert resolve_persona_path("sib.md", search=[far]) == Path("sib.md")

    def test_the_error_lists_every_place_it_looked_and_no_more(self, tmp_path):
        """A relative name was tried in each search directory, so the error has
        to name them all. An absolute path means one place — ``Path(d) /
        "/abs/x.md"`` already discards ``d``, so resolution is correct either
        way, but listing the same path three times reads as a bug in the search
        rather than a missing file."""
        with pytest.raises(FileNotFoundError) as caught:
            resolve_persona_path("ghost.md", search=[tmp_path])
        message = str(caught.value)
        assert "ghost.md" in message
        assert str(tmp_path / "ghost.md") in message

        with pytest.raises(FileNotFoundError) as caught:
            resolve_persona_path("/nope/x.md", search=[tmp_path])
        assert str(caught.value).count("/nope/x.md") == 2  # subject + one try


class TestBuiltinEnumeration:
    def test_it_reads_the_directory_and_every_file_in_it_loads(self):
        """Asserted as a property, not a list: naming the built-ins here would
        recreate the defect the enumeration exists to remove. A persona that
        ships but does not parse would otherwise only fail in a real run."""
        import kerness

        on_disk = {
            p.stem
            for p in (Path(kerness.__file__).parent / "personas").glob("*.md")
        }
        assert list_builtin_personas() == sorted(on_disk)
        assert on_disk

        for name in list_builtin_personas():
            config = load_persona(f"{name}.md")
            assert config.name
            assert config.persona
