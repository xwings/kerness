"""Tests for kerness.agent."""

import os
import tempfile

import pytest

from kerness.agent import Agent


class TestBuildSystemPrompt:
    def test_an_undecorated_agent_gets_the_prompt_it_was_given(self):
        assert Agent(name="Alice", model="test/model").build_system_prompt(
            "You are helpful."
        ) == "You are helpful."

        custom = Agent(name="Alice", model="test/model",
                       system_prompt="Custom prompt.")
        assert custom.build_system_prompt("You are helpful.") == "Custom prompt."

    def test_every_decoration_is_appended_to_the_base(self):
        agent = Agent(name="Alice", model="test/model",
                      persona="Pragmatic engineer", language="French")
        result = agent.build_system_prompt(
            "Base.", skills_prompt="Use summarize skill."
        )

        assert "Persona: Pragmatic engineer" in result
        assert "Respond in French." in result
        assert "Use summarize skill." in result

    def test_show_reasoning_says_yes_no_or_nothing_at_all(self):
        """``None`` is not ``False`` — it leaves the subject unmentioned rather
        than telling the model to keep quiet."""
        agent = Agent(name="Alice", model="test/model")

        assert "Provide brief reasoning." in agent.build_system_prompt(
            "Base.", show_reasoning=True)
        assert "Do not include your reasoning" in agent.build_system_prompt(
            "Base.", show_reasoning=False)
        assert "reasoning" not in agent.build_system_prompt(
            "Base.", show_reasoning=None)

    def test_placeholder_substitution(self):
        agent = Agent(name="Alice", model="test/model")
        result = agent.build_system_prompt("Hello {bot_name}, model={model}")
        assert "Hello Alice" in result
        assert "model=test/model" in result

    def test_persona_from_md_file(self):
        """Persona ending in .md loads from file."""
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".md", delete=False
        ) as f:
            f.write(
                "# Persona: TestBot\n\n"
                "## Persona\nA test persona.\n\n"
                "## Background\nTest background.\n\n"
                "## Communication Style\nDirect.\n"
            )
            f.flush()
            agent = Agent(name="Alice", model="test/model", persona=f.name)
            result = agent.build_system_prompt("Base.")
            assert "Persona: A test persona." in result
            assert "Background: Test background." in result
            assert "Communication style: Direct." in result
        os.unlink(f.name)

    def test_a_missing_persona_file_raises_and_names_what_it_tried(self):
        """Passing the path through as prose would produce a system prompt
        containing the literal line ``Persona: typo.md`` and a session that ran
        to completion looking healthy. Personas fail like gameplans and skills:
        loudly."""
        agent = Agent(name="Alice", model="test/model", persona="typo.md")
        with pytest.raises(FileNotFoundError, match="Tried:"):
            agent.build_system_prompt("Base.")

    def test_a_plain_prose_persona_is_untouched(self):
        """Only strings that claim to be files must resolve. A persona written
        inline has no path to fail on."""
        agent = Agent(name="Alice", model="test/model",
                      persona="A sceptic who asks for evidence.")
        result = agent.build_system_prompt("Base.")
        assert "Persona: A sceptic who asks for evidence." in result


class TestBuildMessages:
    def test_the_system_prompt_leads_and_history_follows_in_order(self):
        agent = Agent(name="Alice", model="test/model")
        msgs = agent.build_messages(
            [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "second"},
            ],
            "Default prompt.",
        )

        assert len(msgs) == 3
        assert msgs[0] == {"role": "system", "content": "Default prompt."}
        assert msgs[1] == {"role": "user", "content": "first"}
        assert msgs[2] == {"role": "assistant", "content": "second"}


class TestRoleChecks:
    def test_an_agent_on_its_own_is_always_a_participant(self):
        """``role`` is the spec and ``position`` is the chair it selects, and
        only ``Session.add_agent`` reads one into the other. An agent that has
        been constructed but added nowhere therefore sits in no session at all,
        which is a participant — the safe answer, and the one that keeps the
        conductor's seat something a session grants rather than something a
        constructor claims."""
        orchestrator = Agent(name="Orch", model="test/model", role="orchestrator")

        assert orchestrator.role == "orchestrator"
        assert orchestrator.position == "participant"
        assert orchestrator.is_participant is True
        assert orchestrator.is_orchestrator is False

    def test_an_unnamed_role_is_none_rather_than_a_default_string(self):
        """Unset has to be distinguishable from ``"participant"`` written out:
        the first takes the built-in role's prompt, the second is prose the
        agent chose for itself."""
        assert Agent(name="Alice", model="test/model").role is None
        assert Agent(name="Alice", model="test/model").position == "participant"
        assert Agent(name="Alice", model="test/model").is_participant is True

    def test_any_string_is_a_role_because_prose_is_one(self):
        """There is nothing to reject here. A role that is not a built-in name
        and not a ``.md`` path is that agent's job written out, and prose seats
        a participant however much it sounds like the other chair."""
        agent = Agent(name="Mod", model="test/model", role="orchestrator, but sceptical")

        assert agent.role == "orchestrator, but sceptical"
        assert agent.position == "participant"

    def test_position_is_read_only(self):
        """It is derived from ``role`` at ``add_agent``, so writing it would be
        writing an answer the session is about to overwrite — or worse, would
        not."""
        with pytest.raises(AttributeError):
            Agent(name="Mod", model="test/model").position = "orchestrator"


class TestReasoningEffort:
    def test_the_level_is_unset_until_named_and_round_trips_as_its_name(self):
        """It sits beside the model because it is chosen with the model.

        Unset is ``None``, not ``"high"``: the session's level fills it at
        ``run()``, and a default written in here would shadow that."""
        assert Agent(name="Alice", model="test/model").reasoning_effort is None

        agent = Agent(name="Alice", model="test/model", reasoning_effort="minimal")
        assert agent.reasoning_effort == "minimal"

        agent.reasoning_effort = "xhigh"
        assert agent.reasoning_effort == "xhigh"

    def test_an_unknown_level_is_rejected(self):
        """Caught where it was written rather than as a 400 on the first turn."""
        with pytest.raises(ValueError, match="Unknown reasoning effort"):
            Agent(name="Alice", model="test/model", reasoning_effort="thorough")

        agent = Agent(name="Alice", model="test/model")
        with pytest.raises(ValueError, match="Unknown reasoning effort"):
            agent.reasoning_effort = "HIGH"
