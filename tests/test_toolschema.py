"""Tests for kerness.toolschema."""

from kerness.provider import ProviderResponse
from kerness.tooling import ToolCall, ToolSpec
from kerness.toolkit import ToolResult
from kerness.toolschema import (
    ToolDialect,
    parse_anthropic_tool_calls,
    parse_openai_tool_calls,
    render_assistant_turn,
    render_tool_result,
    to_anthropic_tool,
    to_openai_tool,
    tool_schemas,
)

SCHEMA = {
    "type": "object",
    "properties": {"command": {"type": "string"}},
    "required": ["command"],
}

CMD = ToolSpec(
    name="cmd",
    description="Run a shell command.",
    parameters=SCHEMA,
    handler=lambda args: "",
)


class TestConversion:
    def test_the_dialect_picks_a_converter_and_they_are_not_interchangeable(self):
        """Different key, different nesting."""
        assert tool_schemas(ToolDialect.OPENAI, [CMD]) == [{
            "type": "function",
            "function": {
                "name": "cmd",
                "description": "Run a shell command.",
                "parameters": SCHEMA,
            },
        }]
        assert tool_schemas(ToolDialect.ANTHROPIC, [CMD]) == [{
            "name": "cmd",
            "description": "Run a shell command.",
            "input_schema": SCHEMA,
        }]
        assert to_openai_tool(CMD) == tool_schemas(ToolDialect.OPENAI, [CMD])[0]
        assert to_anthropic_tool(CMD) == tool_schemas(ToolDialect.ANTHROPIC, [CMD])[0]

    def test_nothing_is_sent_under_text_or_with_no_tools(self):
        """An empty list must not become `tools: []`, which OpenAI rejects."""
        assert tool_schemas(ToolDialect.TEXT, [CMD]) is None
        assert tool_schemas(ToolDialect.OPENAI, []) is None


class TestParsingOpenAI:
    def test_arguments_arrive_as_a_json_string(self):
        message = {
            "content": None,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "cmd", "arguments": '{"command":"ls"}'},
            }],
        }
        assert parse_openai_tool_calls(message) == [
            ToolCall(name="cmd", arguments={"command": "ls"}, id="call_1")
        ]

        assert parse_openai_tool_calls({"content": "hello"}) == []

        several = {"tool_calls": [
            {"id": "a", "function": {"name": "cmd", "arguments": "{}"}},
            {"id": "b", "function": {"name": "read_file", "arguments": "{}"}},
        ]}
        assert [c.name for c in parse_openai_tool_calls(several)] == ["cmd", "read_file"]

    def test_malformed_arguments_are_kept_not_dropped(self):
        """The dispatcher turns this into a schema error the model can fix."""
        message = {"tool_calls": [{
            "id": "c1", "function": {"name": "cmd", "arguments": "{not json"},
        }]}
        assert parse_openai_tool_calls(message)[0].arguments == {"raw": "{not json"}


class TestParsingAnthropic:
    def test_arguments_arrive_as_an_object(self):
        response = {"content": [
            {"type": "text", "text": "Let me check."},
            {"type": "tool_use", "id": "tu_1", "name": "cmd",
             "input": {"command": "ls"}},
        ]}
        assert parse_anthropic_tool_calls(response) == [
            ToolCall(name="cmd", arguments={"command": "ls"}, id="tu_1")
        ]

        text_only = {"content": [{"type": "text", "text": "hello"}]}
        assert parse_anthropic_tool_calls(text_only) == []


class TestRenderingTheAssistantTurn:
    """Both APIs 400 if the turn that made the calls is not replayed."""

    def test_openai_replays_tool_calls_with_string_arguments(self):
        response = ProviderResponse(
            content="",
            tool_calls=[ToolCall("cmd", {"command": "ls"}, id="c1")],
        )
        rendered = render_assistant_turn(ToolDialect.OPENAI, response)
        assert rendered["content"] is None
        assert rendered["tool_calls"][0]["function"]["arguments"] == '{"command": "ls"}'

    def test_anthropic_replays_text_and_tool_use_blocks(self):
        response = ProviderResponse(
            content="Checking.",
            tool_calls=[ToolCall("cmd", {"command": "ls"}, id="tu_1")],
        )
        rendered = render_assistant_turn(ToolDialect.ANTHROPIC, response)
        assert rendered["content"] == [
            {"type": "text", "text": "Checking."},
            {"type": "tool_use", "id": "tu_1", "name": "cmd",
             "input": {"command": "ls"}},
        ]

    def test_text_replays_the_reply_verbatim(self):
        response = ProviderResponse(content="```tool_calls\n{}\n```")
        assert render_assistant_turn(ToolDialect.TEXT, response) == {
            "role": "assistant", "content": "```tool_calls\n{}\n```"
        }


class TestRenderingResults:
    def test_openai_uses_a_tool_role_message(self):
        result = ToolResult(name="cmd", content="file.txt")
        rendered = render_tool_result(
            ToolDialect.OPENAI, ToolCall("cmd", {}, id="c1"), result
        )
        assert rendered == {
            "role": "tool", "tool_call_id": "c1", "content": "file.txt"
        }

    def test_anthropic_uses_a_user_message_carrying_the_error_flag_natively(self):
        assert render_tool_result(
            ToolDialect.ANTHROPIC,
            ToolCall("cmd", {}, id="tu_1"),
            ToolResult(name="cmd", content="file.txt"),
        ) == {
            "role": "user",
            "content": [{
                "type": "tool_result", "tool_use_id": "tu_1",
                "content": "file.txt", "is_error": False,
            }],
        }

        failed = render_tool_result(
            ToolDialect.ANTHROPIC,
            ToolCall("cmd", {}, id="tu_1"),
            ToolResult(name="cmd", content="denied", is_error=True),
        )
        assert failed["content"][0]["is_error"] is True
        assert failed["content"][0]["content"] == "denied"

    def test_dialects_without_an_error_flag_mark_it_inline(self):
        result = ToolResult(name="cmd", content="denied", is_error=True)
        for dialect in (ToolDialect.OPENAI, ToolDialect.TEXT):
            rendered = render_tool_result(dialect, ToolCall("cmd", {}, id="c1"), result)
            assert "[ToolError] denied" in rendered["content"]

    def test_text_rendering_is_unchanged_from_before_native_calling(self):
        """This shape is frozen; the session suite asserts against it."""
        result = ToolResult(name="cmd", content="file.txt")
        assert render_tool_result(
            ToolDialect.TEXT, ToolCall("cmd", {}), result
        ) == {"role": "assistant", "content": "[Tool:cmd] file.txt"}
