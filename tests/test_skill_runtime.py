"""Tests for kerness.skill_runtime — progressive disclosure and gating."""

import pytest

from kerness.exceptions import SessionError
from kerness.skill_loader import SkillConfig
from kerness.skill_runtime import (
    SKILL_TOOL_NAME,
    SkillActivation,
    SkillRegistry,
    apply_gate,
    format_skills_index,
)
from kerness.tooling import ToolSpec


def skill(name, description="Does a thing.", content="BODY", **kwargs):
    return SkillConfig(name=name, description=description, content=content, **kwargs)


def tool(name):
    return ToolSpec(
        name=name, description=name,
        parameters={"type": "object", "properties": {}},
        handler=lambda args: "",
    )


def activation(*skills, grant=None):
    return SkillActivation({s.name: s for s in skills}, grant)


class TestIndex:
    def test_one_line_per_skill_naming_the_body_but_not_carrying_it(self):
        """Names and descriptions only, plus how to fetch the rest — the whole
        point of the milestone."""
        index = format_skills_index([skill("a", "Alpha."), skill("b", "Beta.")])

        assert "- a: Alpha." in index
        assert "- b: Beta." in index
        assert "BODY" not in index
        assert SKILL_TOOL_NAME in index

    def test_no_skills_renders_nothing(self):
        assert format_skills_index([]) == ""


class TestActivation:
    def test_the_body_is_served_once_per_turn(self):
        """A reload inside one turn is answered without repeating the text, but
        the body is scoped to that turn — the next one pays for it again."""
        cfg = skill("a", content="FULL TEXT")
        act = activation(cfg)

        assert act.load("a") == "FULL TEXT"
        second = act.load("a")
        assert "FULL TEXT" not in second
        assert "Already loaded" in second

        assert activation(cfg).load("a") == "FULL TEXT"

    def test_an_unavailable_skill_names_what_is_available(self):
        with pytest.raises(SessionError, match="Available to you: a, b"):
            activation(skill("a"), skill("b")).load("nope")
        with pytest.raises(SessionError, match=r"\(none\)"):
            activation().load("a")


class TestGate:
    def test_only_a_skill_declaring_allowed_tools_narrows(self):
        act = activation(skill("a"))
        assert act.gate is None
        act.load("a")
        assert act.gate is None

    def test_loading_narrows_to_the_declared_tools(self):
        act = activation(skill("a", allowed_tools=("read_file",)))
        act.load("a")
        assert act.gate == {"read_file"}

    def test_an_explicit_empty_list_permits_nothing(self):
        """`allowed-tools: []` is a real answer, not the same as absent."""
        act = activation(skill("a", allowed_tools=()))
        act.load("a")
        assert act.gate == set()

    def test_two_skills_union_rather_than_intersect(self):
        """Loading a second skill must not silently disable the first."""
        act = activation(
            skill("a", allowed_tools=("read_file",)),
            skill("b", allowed_tools=("cmd",)),
        )
        act.load("a")
        act.load("b")
        assert act.gate == {"read_file", "cmd"}

    def test_apply_gate_is_restrictive_only(self):
        """A skill can never grant a tool the agent's toolkit lacks, and no gate
        at all passes the toolkit through untouched."""
        assert [
            t.name for t in apply_gate([tool("read_file")], {"read_file", "cmd"})
        ] == ["read_file"]

        tools = [tool("cmd"), tool("read_file")]
        assert apply_gate(tools, None) == tools

    def test_the_skill_tool_is_never_gated_out(self):
        """An agent under a narrow skill must still be able to load another."""
        tools = [tool("cmd"), tool(SKILL_TOOL_NAME)]
        assert [t.name for t in apply_gate(tools, set())] == [SKILL_TOOL_NAME]


class TestSkillTool:
    def test_the_enum_is_this_agent_s_skills_and_the_handler_loads_them(self):
        act = activation(skill("a", content="FULL TEXT"), skill("b"))
        spec = SkillRegistry(lambda n: []).build_tool(act)

        assert spec.parameters["properties"]["name"]["enum"] == ["a", "b"]
        assert spec.handler({"name": "a"}) == "FULL TEXT"

    def test_no_tool_when_the_agent_has_no_skills(self):
        """An empty enum is not a valid schema, and the tool could only fail."""
        assert SkillRegistry(lambda n: []).build_tool(activation()) is None

    def test_registry_resolves_per_agent(self):
        registry = SkillRegistry(lambda name: [skill(name.lower())])
        assert registry.activation_for("Alice").names == ["alice"]
        assert registry.activation_for("Bob").names == ["bob"]


class TestBundles:
    def _bundled(self, tmp_path, *, builtin):
        base = tmp_path / "demo"
        (base / "scripts").mkdir(parents=True)
        (base / "scripts" / "run.sh").write_text("echo hi\n")
        return skill("demo", base_dir=base, builtin=builtin)

    def test_a_builtin_bundle_is_listed_and_granted(self, tmp_path):
        granted = []
        act = activation(
            self._bundled(tmp_path, builtin=True), grant=granted.extend
        )
        result = act.load("demo")

        assert "Bundled resources:" in result
        assert [p.name for p in granted] == ["scripts"]

    def test_an_untrusted_bundle_is_listed_but_not_granted(self, tmp_path):
        """Activating a skill from an arbitrary path must not widen access."""
        granted = []
        act = activation(
            self._bundled(tmp_path, builtin=False), grant=granted.extend
        )
        result = act.load("demo")

        assert "access was not granted" in result
        assert granted == []

    def test_no_manifest_when_there_is_no_bundle(self, tmp_path):
        base = tmp_path / "plain"
        base.mkdir()
        act = activation(skill("plain", base_dir=base, builtin=True))
        assert act.load("plain") == "BODY"

    def test_a_pathless_skill_has_no_bundles(self):
        assert skill("a").bundle_paths() == []
