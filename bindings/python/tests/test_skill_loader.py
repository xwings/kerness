"""Tests for kerness.skill_loader."""

import os
import tempfile

import pytest

from kerness.skill_loader import list_builtin_skills, load_skill


class TestLoadSkill:
    def test_a_builtin_loads_by_bare_name_or_by_file(self):
        skill = load_skill("summarize")
        assert skill.name  # parsed from frontmatter
        assert skill.description
        assert load_skill("summarize/SKILL.md").name == skill.name

    def test_a_missing_skill_is_a_file_not_found_either_way(self):
        with pytest.raises(FileNotFoundError, match="not found"):
            load_skill("nonexistent_skill")
        with pytest.raises(FileNotFoundError, match="not found"):
            load_skill("/nonexistent/custom_skill.md")

    def test_a_custom_skill_loads_from_an_absolute_or_relative_path(self):
        def write(parent, name):
            skill_dir = os.path.join(parent, name)
            os.makedirs(skill_dir, exist_ok=True)
            path = os.path.join(skill_dir, "SKILL.md")
            with open(path, "w", encoding="utf-8") as f:
                f.write(f"---\nname: {name}\ndescription: A custom skill.\n---\n")
            return path

        with tempfile.TemporaryDirectory() as tmpdir:
            skill = load_skill(write(tmpdir, "custom-skill"))
            assert skill.name == "custom-skill"
            assert skill.description == "A custom skill."

        with tempfile.TemporaryDirectory(dir=".") as tmpdir:
            path = write(tmpdir, "relative-skill")
            assert load_skill(path).name == "relative-skill"


class TestListBuiltinSkills:
    def test_returns_the_known_skills_sorted(self):
        skills = list_builtin_skills()
        assert {"summarize", "fact-check", "challenge"} <= set(skills)
        assert skills == sorted(skills)


class TestAllowedTools:
    def _skill(self, tmp_path, frontmatter):
        base = tmp_path / "demo"
        base.mkdir(parents=True)
        (base / "SKILL.md").write_text(
            f"---\nname: demo\ndescription: A demo skill.\n{frontmatter}---\n\nBody.\n"
        )
        return load_skill(str(base / "SKILL.md"))

    def test_absent_narrows_nothing_and_an_empty_list_narrows_to_nothing(self, tmp_path):
        """Collapsing the two would silently grant every tool to a skill that
        declared it wanted none."""
        assert self._skill(tmp_path / "a", "").allowed_tools is None
        assert self._skill(tmp_path / "b", "allowed-tools: []\n").allowed_tools == ()

    def test_inline_and_block_lists_both_parse(self, tmp_path):
        """Why the frontmatter goes through yaml.safe_load: a parser that split
        on the first ':' could not represent either form."""
        inline = self._skill(tmp_path / "a", "allowed-tools: [read_file, cmd]\n")
        block = self._skill(tmp_path / "b", "allowed-tools:\n  - read_file\n  - cmd\n")
        assert inline.allowed_tools == ("read_file", "cmd")
        assert block.allowed_tools == ("read_file", "cmd")

    def test_a_non_list_and_malformed_yaml_are_both_reported(self, tmp_path):
        with pytest.raises(ValueError, match="allowed-tools must be a list"):
            self._skill(tmp_path / "a", "allowed-tools: {a: b}\n")
        with pytest.raises(ValueError, match="Invalid YAML"):
            self._skill(tmp_path / "b", "allowed-tools: [unclosed\n")

    def test_requires_tools_is_a_plain_tuple_with_no_absent_state(self, tmp_path):
        """The counterpart key, and the one place the two differ: a skill that
        named no requirement and one that named an empty list both require
        nothing, so there is no second state to keep apart."""
        assert self._skill(tmp_path / "a", "").requires_tools == ()
        assert self._skill(tmp_path / "b", "requires-tools: []\n").requires_tools == ()
        assert self._skill(
            tmp_path / "c", "requires-tools: [cmd]\n"
        ).requires_tools == ("cmd",)
        with pytest.raises(ValueError, match="requires-tools must be a list"):
            self._skill(tmp_path / "d", "requires-tools: {a: b}\n")


class TestBundleDiscovery:
    def test_only_builtin_skills_are_marked_builtin(self, tmp_path):
        """The flag is what grants bundle access, so a path-loaded skill
        claiming it would widen the grant to any directory on disk."""
        assert load_skill("summarize").builtin is True

        base = tmp_path / "demo"
        base.mkdir()
        (base / "SKILL.md").write_text(
            "---\nname: demo\ndescription: A demo.\n---\n\nBody.\n"
        )
        assert load_skill(str(base / "SKILL.md")).builtin is False

    def test_only_existing_bundle_dirs_are_reported(self, tmp_path):
        base = tmp_path / "demo"
        (base / "references").mkdir(parents=True)
        (base / "SKILL.md").write_text(
            "---\nname: demo\ndescription: A demo.\n---\n\nBody.\n"
        )
        skill = load_skill(str(base / "SKILL.md"))
        assert [p.name for p in skill.bundle_paths()] == ["references"]
