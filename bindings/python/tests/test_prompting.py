"""Tests for kerness.prompting."""

import pytest

from kerness.agent import Agent
from kerness.memory import Memory
from kerness.prompting import MEMORY_HEADER, PromptAssembler, memory_block
from kerness.tooling import ToolSpec
from kerness.toolschema import ToolDialect


def _loaded(path) -> Memory:
    """Memory.read() serves a cache that load() fills, as Session.run() does."""
    memory = Memory(str(path))
    memory.load()
    return memory


@pytest.fixture
def empty_memory(tmp_path):
    # load() writes a '# Memory' template, so an empty *file* still renders a
    # block. Truly-empty content is the case that must render nothing.
    path = tmp_path / "empty.md"
    path.write_text("")
    return _loaded(path)


@pytest.fixture
def filled_memory(tmp_path):
    path = tmp_path / "filled.md"
    path.write_text("# Memory\n- a prior note\n")
    return _loaded(path)


def ping_tool():
    return ToolSpec(
        name="ping",
        description="Ping tool",
        parameters={"type": "object", "properties": {}},
        handler=lambda args: "pong",
    )


def assembler(**overrides):
    defaults = dict(
        skills_for=lambda a: "",
        memory_for=lambda a: None,
        tools_for=list,
        show_reasoning=None,
    )
    defaults.update(overrides)
    return PromptAssembler(**defaults)


class TestMemoryBlock:
    def test_memory_with_nothing_in_it_renders_nothing(self, empty_memory, tmp_path):
        assert memory_block(empty_memory) == ""

        path = tmp_path / "ws.md"
        path.write_text("   \n\n  \n")
        assert memory_block(Memory(str(path))) == ""

    def test_only_a_writable_session_invites_notes(self, filled_memory):
        """Asking for notes a read-only session discards is a false promise."""
        block = memory_block(filled_memory)
        assert block.startswith(MEMORY_HEADER)
        assert "a prior note" in block
        assert "@MEMORY:" not in block
        assert "write_memory" not in block

        writable = memory_block(filled_memory, writable=True)
        assert "@MEMORY:" in writable
        assert "write_memory" in writable


class TestOrchestratorPrompt:
    def test_order_is_base_skills_tools_memory(self, filled_memory):
        a = assembler(
            skills_for=lambda ag: "SKILLS_BLOCK",
            memory_for=lambda ag: filled_memory,
            tools_for=lambda: [ping_tool()],
        )
        agent = Agent(name="Mod", model="m", role="orchestrator")
        prompt = a.orchestrator_system(agent, "BASE")

        assert prompt.index("BASE") < prompt.index("SKILLS_BLOCK")
        assert prompt.index("SKILLS_BLOCK") < prompt.index("Tool definitions:")
        assert prompt.index("Tool definitions:") < prompt.index("## Memory")

    def test_no_skills_no_tools_no_memory_is_just_the_base(self, empty_memory):
        a = assembler(memory_for=lambda ag: empty_memory)
        agent = Agent(name="Mod", model="m", role="orchestrator")
        assert a.orchestrator_system(agent, "BASE") == "BASE"

    def test_history_follows_the_system_message_and_is_not_mutated(self, empty_memory):
        a = assembler(memory_for=lambda ag: empty_memory)
        agent = Agent(name="Mod", model="m", role="orchestrator")
        history = [{"role": "user", "content": "topic"}]
        messages = a.messages_for(agent, history, "BASE")

        assert messages[0] == {"role": "system", "content": "BASE"}
        assert messages[1:] == history
        assert history == [{"role": "user", "content": "topic"}]


class TestParticipantPrompt:
    def test_persona_and_language_survive(self, empty_memory):
        a = assembler(memory_for=lambda ag: empty_memory)
        agent = Agent(name="Bob", model="m", persona="Engineer", language="French")
        system = a.participant_messages(agent, [], "BASE")[0]["content"]

        assert "Persona: Engineer" in system
        assert "Respond in French." in system

    def test_memory_rides_with_skills_before_tools(self, filled_memory):
        a = assembler(
            skills_for=lambda ag: "SKILLS_BLOCK",
            memory_for=lambda ag: filled_memory,
            tools_for=lambda: [ping_tool()],
        )
        agent = Agent(name="Bob", model="m")
        system = a.participant_messages(agent, [], "BASE")[0]["content"]

        assert system.index("SKILLS_BLOCK") < system.index("## Memory")
        assert system.index("## Memory") < system.index("Tool definitions:")

    def test_agent_system_prompt_overrides_the_default(self, empty_memory):
        a = assembler(memory_for=lambda ag: empty_memory)
        agent = Agent(name="Bob", model="m", system_prompt="CUSTOM")
        system = a.participant_messages(agent, [], "DEFAULT")[0]["content"]

        assert "CUSTOM" in system
        assert "DEFAULT" not in system

    def test_show_reasoning_reaches_the_participant_prompt(self, empty_memory):
        """``None`` says nothing at all, which is not the same as saying no."""
        agent = Agent(name="Bob", model="m")

        def system(flag):
            a = assembler(memory_for=lambda ag: empty_memory, show_reasoning=flag)
            return a.participant_messages(agent, [], "BASE")[0]["content"]

        assert "Provide brief reasoning." in system(True)
        assert "Do not include your reasoning, only the answer." in system(False)
        assert "reasoning" not in system(None).lower()


class TestRoleDispatch:
    def test_messages_for_routes_by_role(self, empty_memory):
        a = assembler(memory_for=lambda ag: empty_memory)
        orchestrator = Agent(name="Mod", model="m", role="orchestrator")
        participant = Agent(name="Bob", model="m", persona="Engineer")

        # The orchestrator's prompt is used verbatim; a participant's is composed.
        assert a.messages_for(orchestrator, [], "BASE")[0]["content"] == "BASE"
        assert "Persona: Engineer" in a.messages_for(participant, [], "BASE")[0]["content"]

    def test_tools_reflect_the_current_permitted_set(self, empty_memory):
        """tools_for is read per call, so harness narrowing is picked up."""
        permitted: list[ToolSpec] = []
        a = assembler(memory_for=lambda ag: empty_memory, tools_for=lambda: permitted)
        agent = Agent(name="Mod", model="m", role="orchestrator")

        assert "Tool definitions:" not in a.orchestrator_system(agent, "BASE")
        permitted.append(ping_tool())
        assert "Tool definitions:" in a.orchestrator_system(agent, "BASE")


class TestDialectAwareness:
    """Under a native dialect the schemas ride in the request, not the prompt."""

    def test_a_native_dialect_drops_the_prose_tools_block(self, empty_memory):
        agent = Agent(name="Mod", model="m", role="orchestrator")

        def system(dialect):
            a = assembler(
                memory_for=lambda ag: empty_memory,
                tools_for=lambda: [ping_tool()],
                dialect_for=lambda ag: dialect,
            )
            return a.orchestrator_system(agent, "BASE")

        native = system(ToolDialect.OPENAI)
        assert "Tool definitions:" not in native
        # The fence instruction is the harmful half: a native model told to
        # answer in a fence will do that instead of calling properly.
        assert "tool_calls" not in native

        assert "Tool definitions:" in system(ToolDialect.TEXT)

    def test_no_resolver_means_text(self, empty_memory):
        """A caller with no providers in hand — the suite, mostly — sees TEXT."""
        a = assembler(memory_for=lambda ag: empty_memory, tools_for=lambda: [ping_tool()])
        agent = Agent(name="Bob", model="m")
        system = a.participant_messages(agent, [], "BASE")[0]["content"]
        assert "Tool definitions:" in system

    def test_the_dialect_is_resolved_per_agent(self, empty_memory):
        """Mixed-provider sessions are supported, so this cannot be session-wide.
        Participants are gated by it exactly as the orchestrator is."""
        dialects = {"Native": ToolDialect.ANTHROPIC, "Fenced": ToolDialect.TEXT}
        a = assembler(
            memory_for=lambda ag: empty_memory,
            tools_for=lambda: [ping_tool()],
            dialect_for=lambda ag: dialects[ag.name],
        )
        native = a.participant_messages(Agent(name="Native", model="m"), [], "B")
        fenced = a.participant_messages(Agent(name="Fenced", model="m"), [], "B")

        assert "Tool definitions:" not in native[0]["content"]
        assert "Tool definitions:" in fenced[0]["content"]
