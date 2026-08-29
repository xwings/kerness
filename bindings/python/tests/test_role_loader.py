"""Tests for kerness.role_loader."""

from pathlib import Path

import pytest

from kerness.role_loader import (
    DEFAULT_ROLE_FILE,
    RoleConfig,
    list_builtin_roles,
    load_role,
    resolve_role_path,
)


class TestLoadRole:
    def test_frontmatter_and_body_are_both_read(self, tmp_path):
        path = tmp_path / "judge.md"
        path.write_text(
            "---\nname: judge\nposition: orchestrator\n"
            "description: Runs the room.\n---\n\nYou judge.\n",
            encoding="utf-8",
        )
        config = load_role(str(path))

        assert config.name == "judge"
        assert config.position == "orchestrator"
        assert config.description == "Runs the room."
        assert config.content == "You judge."

    def test_a_file_with_no_frontmatter_is_a_participant_named_for_itself(
        self, tmp_path
    ):
        """The smallest useful role is one paragraph in a file. Defaulting the
        position the other way would hand the conductor's seat to anyone who
        wrote one."""
        path = tmp_path / "sceptic.md"
        path.write_text("Doubt everything.\n", encoding="utf-8")
        config = load_role(str(path))

        assert config.name == "sceptic"
        assert config.position == "participant"
        assert config.content == "Doubt everything."

    def test_an_unknown_position_is_refused_and_names_its_file(self, tmp_path):
        """``position`` is the one field the framework acts on, so a value it
        does not know is an error rather than a silent participant."""
        path = tmp_path / "odd.md"
        path.write_text("---\nposition: referee\n---\n\nBody.\n", encoding="utf-8")

        with pytest.raises(ValueError) as caught:
            load_role(str(path))
        assert "referee" in str(caught.value)
        assert "odd.md" in str(caught.value)


class TestTheSearchPath:
    def test_a_bare_name_finds_the_builtin(self):
        assert resolve_role_path("orchestrator.md").exists()

    def test_a_search_directory_is_tried_by_resolve_and_by_load(self, tmp_path):
        (tmp_path / "chair.md").write_text(
            "---\nposition: orchestrator\n---\n\nMine.\n", encoding="utf-8"
        )

        assert resolve_role_path("chair.md", search=[tmp_path]) == tmp_path / "chair.md"
        assert load_role("chair.md", search=[tmp_path]).content == "Mine."

    def test_the_error_lists_every_place_it_looked(self, tmp_path):
        with pytest.raises(FileNotFoundError) as caught:
            resolve_role_path("ghost.md", search=[tmp_path])
        message = str(caught.value)
        assert "ghost.md" in message
        assert str(tmp_path / "ghost.md") in message


class TestRoleConfig:
    def test_a_position_it_cannot_act_on_is_refused_at_construction(self):
        """Same closed set as the file loader enforces. A ``RoleConfig`` built
        in Python is fed to the same code, so it cannot be the looser door."""
        assert RoleConfig().position == "participant"
        assert RoleConfig(name="chair", position="orchestrator").position == "orchestrator"

        with pytest.raises(ValueError, match="Unknown agent position"):
            RoleConfig(name="chair", position="moderator")


class TestBuiltinEnumeration:
    def test_it_reads_the_directory_and_every_file_in_it_loads(self):
        """Asserted as a property, not a list: naming the built-ins here would
        recreate the defect the enumeration exists to remove."""
        import kerness

        on_disk = {
            p.stem for p in (Path(kerness.__file__).parent / "roles").glob("*.md")
        }
        assert list_builtin_roles() == sorted(on_disk)
        assert on_disk

        for name in list_builtin_roles():
            config = load_role(f"{name}.md")
            assert config.name
            assert config.description
            assert config.content

    def test_the_default_role_is_one_of_them(self):
        """An agent that names no role reads this file, so it has to be a role
        that ships and it has to seat a participant."""
        assert DEFAULT_ROLE_FILE.removesuffix(".md") in list_builtin_roles()
        assert load_role(DEFAULT_ROLE_FILE).position == "participant"
