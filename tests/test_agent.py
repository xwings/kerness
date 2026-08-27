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
    def test_the_two_roles_are_exclusive_and_participant_is_the_default(self):
        orchestrator = Agent(name="Orch", model="test/model", role="orchestrator")
        participant = Agent(name="Alice", model="test/model", role="participant")

        assert orchestrator.is_orchestrator is True
        assert orchestrator.is_participant is False
        assert participant.is_participant is True
        assert participant.is_orchestrator is False

        assert Agent(name="Alice", model="test/model").is_participant is True

    def test_an_unknown_role_is_rejected(self):
        """The dangerous outcome is not an error — it is the silent one. An
        unrecognised role matches neither the orchestrator lookup nor
        ``is_orchestrator``, so defaulting it to participant would quietly turn
        the session's conductor into an extra debater. This asserts the loud
        path wins."""
        for role in ("orchestrater", "moderator"):
            with pytest.raises(ValueError, match="Unknown agent role"):
                Agent(name="Mod", model="test/model", role=role)
