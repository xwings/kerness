"""Tests for kerness.toolkit."""

from kerness.exceptions import AccessDeniedError
from kerness.tooling import ToolCall, ToolSpec
from kerness.toolkit import INVALID_CALL, ToolDispatcher, ToolResult, resolve


def spec(name="ping", handler=lambda args: "pong", parameters=None, **kwargs):
    return ToolSpec(
        name=name,
        description=f"{name} tool",
        parameters=parameters if parameters is not None
        else {"type": "object", "properties": {}},
        handler=handler,
        **kwargs,
    )


def dispatcher(*tools):
    return ToolDispatcher(lambda: list(tools))


class TestSuccess:
    def test_whatever_the_handler_returns_arrives_as_text(self):
        """The model reads text, but list_dir returns a list and a counter
        returns an int — anything not coerced here reaches the prompt as a
        repr."""
        assert dispatcher(spec()).execute(ToolCall("ping", {})) == ToolResult(
            name="ping", content="pong", is_error=False
        )

        listing = spec(name="ls", handler=lambda args: ["a", "b"])
        assert dispatcher(listing).execute(ToolCall("ls", {})).content == "a, b"

        counter = spec(name="count", handler=lambda args: 42)
        assert dispatcher(counter).execute(ToolCall("count", {})).content == "42"

    def test_the_actor_reaches_only_handlers_that_asked_for_it(self):
        """Tools registered via add_tool take one argument, so passing the
        actor unconditionally would break every caller-supplied tool."""
        seen = []
        asking = spec(
            name="who",
            handler=lambda args, actor: seen.append(actor) or "ok",
            takes_actor=True,
        )
        dispatcher(asking).execute(ToolCall("who", {}), actor="Alice")
        assert seen == ["Alice"]

        plain = dispatcher(spec(handler=lambda args: "pong"))
        assert not plain.execute(ToolCall("ping", {}), actor="Alice").is_error


class TestFailuresBecomeResults:
    """Dispatch never raises; every failure is something the model can read."""

    def test_a_call_that_names_nothing_real_is_reported_back(self):
        """Both are the model's own to correct on its next iteration; raising
        would end the turn over a mistake it could have fixed."""
        unknown = dispatcher(spec()).execute(ToolCall("teleport", {}))
        assert unknown.is_error
        assert "Unknown tool: teleport" in unknown.content

        unparseable = dispatcher(spec()).execute(
            ToolCall(INVALID_CALL, {"error": "unclosed tool_calls fence"})
        )
        assert unparseable.is_error
        assert "unclosed tool_calls fence" in unparseable.content

    def test_arguments_are_checked_against_the_schema(self):
        """Argument-shape guards live here, not in each handler."""
        tool = spec(
            name="cmd",
            handler=lambda args: args["command"],
            parameters={
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            },
        )
        missing = dispatcher(tool).execute(ToolCall("cmd", {}))
        assert missing.is_error
        assert "missing required argument 'command'" in missing.content

        wrong_type = dispatcher(tool).execute(ToolCall("cmd", {"command": 7}))
        assert wrong_type.is_error
        assert "must be string" in wrong_type.content

    def test_a_raising_handler(self):
        def boom(args):
            raise RuntimeError("kaboom")

        result = dispatcher(spec(handler=boom)).execute(ToolCall("ping", {}))
        assert result.is_error
        assert "kaboom" in result.content

    def test_a_denied_command_is_reported_not_raised(self):
        """The agent learns it was denied instead of the turn aborting."""
        def denied(args):
            raise AccessDeniedError("Command denied: rm -rf /")

        result = dispatcher(spec(handler=denied)).execute(ToolCall("ping", {}))
        assert result.is_error
        assert "Command denied" in result.content


class TestResolve:
    def test_absent_is_everything_empty_is_nothing_and_order_is_registration(self):
        """Collapsing absent and empty would grant every tool to a harness that
        declared it wanted none. Ordering by the allow list instead would make
        the prompt's tool block depend on how the caller typed it."""
        tools = [spec("a"), spec("b"), spec("c")]

        assert resolve(tools, None) == tools
        assert resolve(tools, []) == []
        assert [t.name for t in resolve(tools, ["c", "a"])] == ["a", "c"]
