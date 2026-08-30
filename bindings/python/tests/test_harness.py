"""Tests for kerness.harness — the harness contract."""

import pytest

from kerness.exceptions import GameplanLoadError, SessionError
from kerness.harness import (
    HarnessSpec,
    Permitted,
    PhaseSpec,
    parse_harness,
    validate_harness,
)


def parse(data):
    return parse_harness(data, source="test.md")


class TestParseAgents:
    def test_the_orchestrator_key_takes_two_shapes_and_refuses_a_third(self):
        """`orchestrator: true` and the mapping form are the same key spelled
        two ways, so both have to land on the same field; anything else is a
        typo the loader must name rather than coerce."""
        assert parse({}).agents.orchestrator.required is False
        assert parse({"agents": {"orchestrator": True}}).agents.orchestrator.required

        spec = parse(
            {"agents": {"orchestrator": {"required": True, "instruction": "Be brief."}}}
        )
        assert spec.agents.orchestrator.required is True
        assert spec.agents.orchestrator.instruction == "Be brief."

        with pytest.raises(GameplanLoadError, match="bool or a mapping"):
            parse({"agents": {"orchestrator": 3}})

    def test_participant_bounds_default_open_and_parse_when_given(self):
        """An absent `max` is unbounded, not zero — the difference between a
        harness that seats anyone and one that seats nobody."""
        absent = parse({}).agents.participants
        assert (absent.min, absent.max) == (1, None)

        given = parse({"agents": {"participants": {"min": 2, "max": 5}}})
        assert (given.agents.participants.min, given.agents.participants.max) == (2, 5)

    def test_unsatisfiable_bounds_are_rejected(self):
        with pytest.raises(GameplanLoadError, match="below 'min'"):
            parse({"agents": {"participants": {"min": 4, "max": 2}}})
        with pytest.raises(GameplanLoadError, match=">= 1"):
            parse({"agents": {"participants": {"min": 0}}})


class TestParseLoop:
    def test_defaults(self):
        """The judge rethinking its verdict is the shipped behaviour; a harness
        opts out of it, not into it."""
        loop = parse({}).loop
        assert loop.terminate_on == ("END_SESSION",)
        assert loop.max_rounds == 3
        assert loop.phases == ()
        assert loop.verdict_rethink is True

        assert parse({"loop": {"verdict_rethink": False}}).loop.verdict_rethink is False

    def test_a_single_terminator_is_wrapped_and_none_at_all_is_refused(self):
        """A harness that declares no usable keyword could not end, so the
        loader has to say so at load rather than at turn `max_turns`."""
        assert parse({"loop": {"terminate_on": "DONE"}}).loop.terminate_on == ("DONE",)
        with pytest.raises(GameplanLoadError, match="cannot end"):
            parse({"loop": {"terminate_on": []}})
        with pytest.raises(GameplanLoadError, match="no usable keywords"):
            parse({"loop": {"terminate_on": ["  "]}})

    def test_the_consensus_keyword_is_recognised_only_when_declared(self):
        loop = parse({"loop": {"terminate_on": ["END_SESSION", "CONSENSUS_REACHED"]}}).loop
        assert loop.consensus_keyword == "CONSENSUS_REACHED"

        assert parse({"loop": {"terminate_on": ["END_SESSION"]}}).loop.consensus_keyword is None

    def test_a_scalar_of_the_wrong_type_is_rejected_not_coerced(self):
        """``True`` is an int subclass, so a bare bool has to be caught too; and
        YAML's quoted ``"false"`` is a string that must not become true merely
        because non-empty Python strings are truthy."""
        with pytest.raises(GameplanLoadError, match="must be an integer"):
            parse({"loop": {"max_rounds": "many"}})
        with pytest.raises(GameplanLoadError, match="must be an integer"):
            parse({"loop": {"max_rounds": True}})
        with pytest.raises(GameplanLoadError, match="must be a boolean"):
            parse({"loop": {"verdict_rethink": "false"}})


class TestParsePhases:
    def test_phase_fields(self):
        spec = parse(
            {
                "loop": {
                    "phases": [
                        {"name": "think", "instruction": "Alone.", "rounds": 2},
                        {"name": "rethink", "rethink": True},
                    ]
                }
            }
        )
        think, rethink = spec.loop.phases
        assert think == PhaseSpec(name="think", instruction="Alone.", rounds=2)
        assert rethink.rethink is True
        assert rethink.rounds == 1

    @pytest.mark.parametrize("phases, message", [
        ([{"instruction": "x"}], "missing 'name'"),
        ([{"name": "a"}, {"name": "a"}], "Duplicate phase"),
        ([{"name": "Think Hard"}], "lowercase slug"),
        ({"name": "think"}, "must be a list"),
        ([{"name": "think", "rethink": "false"}], "must be a boolean"),
    ])
    def test_a_malformed_phase_list_is_rejected(self, phases, message):
        with pytest.raises(GameplanLoadError, match=message):
            parse({"loop": {"phases": phases}})


class TestToolsNarrowSkillsWiden:
    def test_the_tools_key_narrows_what_was_registered(self):
        """Absent means all, empty means none, named means those — in
        registration order, not in the order the harness happened to list."""
        assert parse({}).resolve_tools(["cmd", "read_file"]) == ["cmd", "read_file"]
        assert parse({"tools": []}).resolve_tools(["cmd"]) == []

        spec = parse({"tools": ["list_dir", "cmd"]})
        assert spec.resolve_tools(["cmd", "read_file", "list_dir"]) == ["cmd", "list_dir"]

    def test_unknown_tool_is_an_error_not_a_silent_drop(self):
        spec = parse({"name": "x", "tools": ["teleport"]})
        with pytest.raises(SessionError, match="teleport"):
            spec.resolve_tools(["cmd"])

    def test_reserved_tool_name_rejected_at_load(self):
        with pytest.raises(GameplanLoadError, match="reserved"):
            parse({"tools": ["Skill"]})

    def test_the_skills_key_unions_with_the_session_and_dedupes(self):
        spec = parse({"skills": ["challenge", "summarize"]})
        assert spec.resolve_skills(["summarize"]) == ["summarize", "challenge"]
        assert spec.resolve_skills(["summarize", "summarize"]) == [
            "summarize",
            "challenge",
        ]

        assert parse({}).resolve_skills(["a"]) == ["a"]


class TestParseResult:
    def test_a_field_takes_the_shorthand_or_the_long_form(self):
        """Both spellings name one type, and a type nobody implements is an
        error at load — not a field that silently never validates."""
        (field,) = parse({"result": {"summary": "str"}}).result
        assert field.name == "summary"
        assert field.py_type is str

        (field,) = parse(
            {"result": {"ok": {"type": "bool", "description": "Did it work."}}}
        ).result
        assert field.py_type is bool
        assert field.description == "Did it work."

        with pytest.raises(GameplanLoadError, match="unknown type"):
            parse({"result": {"x": "widget"}})


class TestParseName:
    def test_a_name_is_optional_but_must_be_a_slug_when_given(self):
        assert parse({}).name == ""
        with pytest.raises(GameplanLoadError, match="lowercase slug"):
            parse({"name": "My Gameplan"})


class TestValidateHarness:
    def spec(self, **kw):
        return parse({"name": "t", **kw})

    def test_passing_session_returns_what_it_permits(self):
        permitted = validate_harness(
            self.spec(tools=["cmd"], context=["repo_map"]),
            participants=["A", "B"],
            orchestrator="M",
            registered_tools=["cmd", "read_file"],
            registered_context=["repo_map", "open_bugs"],
        )
        assert permitted.tools == ("cmd",)
        assert permitted.context == ("repo_map",)
        assert permitted == Permitted(tools=["cmd"], context=["repo_map"])

    def test_registered_context_is_optional_and_defaults_to_none_registered(self):
        """A session that registers no sources is only in trouble if the
        gameplan asked for one."""
        assert validate_harness(
            self.spec(),
            participants=["A"],
            orchestrator=None,
            registered_tools=[],
        ) == Permitted(tools=[], context=[])

        with pytest.raises(SessionError, match="requires context source"):
            validate_harness(
                self.spec(context=["repo_map"]),
                participants=["A"],
                orchestrator=None,
                registered_tools=[],
            )

    def test_the_roster_must_fit_the_declared_bounds(self):
        with pytest.raises(SessionError, match="at least 3"):
            validate_harness(
                self.spec(agents={"participants": {"min": 3}}),
                participants=["A"],
                orchestrator=None,
                registered_tools=[],
            )
        with pytest.raises(SessionError, match="at most 2"):
            validate_harness(
                self.spec(agents={"participants": {"max": 2}}),
                participants=["A", "B", "C"],
                orchestrator=None,
                registered_tools=[],
            )

    def test_every_name_on_the_roster_must_be_distinct(self):
        """Including the orchestrator's — routing is by name, so a clash makes
        an ``@Name`` ambiguous."""
        with pytest.raises(SessionError, match="duplicate agent name"):
            validate_harness(
                self.spec(),
                participants=["A", "A"],
                orchestrator=None,
                registered_tools=[],
            )
        with pytest.raises(SessionError, match="shares a name"):
            validate_harness(
                self.spec(),
                participants=["A"],
                orchestrator="A",
                registered_tools=[],
            )

    def test_every_problem_is_reported_at_once(self):
        """One run, one list — not one error per re-run."""
        with pytest.raises(SessionError) as exc:
            validate_harness(
                self.spec(
                    agents={"orchestrator": True, "participants": {"min": 4}},
                    tools=["teleport"],
                ),
                participants=["A", "A"],
                orchestrator=None,
                registered_tools=["cmd"],
            )
        message = str(exc.value)
        assert "requires an orchestrator" in message
        assert "at least 4" in message
        assert "duplicate agent name" in message
        assert "teleport" in message


class TestDefaults:
    def test_bare_spec_is_usable(self):
        spec = HarnessSpec()
        assert spec.loop.terminate_on == ("END_SESSION",)
        assert spec.resolve_tools(["cmd"]) == ["cmd"]
        assert spec.resolve_skills([]) == []
