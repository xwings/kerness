"""Tests for tool-call parsing compatibility."""

from kerness.tooling import parse_tool_calls
from kerness.toolkit import INVALID_CALL

PAYLOAD = (
    '{"tool_calls":[{"id":"call_1","type":"function",'
    '"function":{"name":"cmd","arguments":"{}"}}]}'
)


def test_a_call_is_found_in_every_wrapper_a_model_reaches_for():
    """Models fence tool calls under ```tool_calls, under ```json, bare, or
    with the channel prefix already glued to the fence — and some emit a plain
    array instead of the tool_calls object. Each shape was observed in a real
    run; missing one silently drops the call and the turn stalls."""
    for text in (
        f"```tool_calls\n{PAYLOAD}\n```\n",
        f"```json\n{PAYLOAD}\n```\n",
        PAYLOAD,
        f"[Bo] ```tool_calls\n{PAYLOAD}\n```\n",
    ):
        calls = parse_tool_calls(text)
        assert len(calls) == 1, text
        assert calls[0].name == "cmd"

    array = (
        "```tool_calls\n[\n"
        '  {"name": "cmd", "arguments": {"command": "one"}},\n'
        '  {"name": "cmd", "arguments": {"command": "two"}}\n'
        "]\n```\n"
    )
    assert [c.arguments["command"] for c in parse_tool_calls(array)] == ["one", "two"]


def test_output_that_merely_looks_like_one_is_left_alone():
    """A ```json block with no tool_calls key is ordinary output — most often
    the result: block kerness.loop asks the orchestrator for. Reading it as a
    malformed call sends an error result back and re-asks forever."""
    assert parse_tool_calls(
        'They agreed.\n\n```json\n{"consensus": true, "summary": "Agreed."}\n```'
    ) == []
    assert parse_tool_calls('{"not_tool_calls": []}') == []


def test_a_payload_with_nothing_callable_in_it_becomes_an_invalid_call():
    """Returning [] instead would read as 'the model said something ordinary'
    and the malformed block would never be fed back for it to fix."""
    unclosed = (
        "[Lead] ```tool_calls\n" + PAYLOAD + "\n```tool_calls\n"
    )
    for text in (
        "```json\n{tool_calls: [}\n```",
        '```tool_calls\n{"tool_calls": []}\n```',
        '```tool_calls\n{"tool_calls": [7, {}]}\n```',
        unclosed,
    ):
        calls = parse_tool_calls(text)
        assert len(calls) == 1, text
        assert calls[0].name == INVALID_CALL
