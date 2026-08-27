"""Tests for kerness.agent_runtime."""

from kerness.agent import Agent
from kerness.agent_runtime import (
    FOLLOWUP_PROMPT,
    MAX_INVALID_CALLS,
    AgentRunner,
)
from kerness.exceptions import ProviderError
from kerness.provider import Provider, ProviderResponse
from kerness.tooling import ToolCall, ToolSpec
from kerness.toolkit import ToolDispatcher
from kerness.toolschema import ToolDialect
from tests.conftest import SequenceMockProvider


def call_block(name: str, arguments: str = "{}") -> str:
    return (
        "```tool_calls\n"
        f'{{"tool_calls":[{{"id":"c1","type":"function",'
        f'"function":{{"name":"{name}","arguments":"{arguments}"}}}}]}}\n'
        "```\n"
    )


def spec(name="ping", handler=lambda args: "pong"):
    return ToolSpec(
        name=name,
        description=f"{name} tool",
        parameters={"type": "object", "properties": {}},
        handler=handler,
    )


def runner(provider, *tools, max_tool_iterations=None, record=None,
           tools_for=None):
    return AgentRunner(
        agent=Agent(name="Alice", model="m"),
        provider=provider,
        messages_for=lambda agent, history, base: [
            {"role": "system", "content": base}, *history
        ],
        dispatcher=ToolDispatcher(lambda: list(tools)),
        base_prompt="BASE",
        max_tool_iterations=max_tool_iterations,
        record_tool_exchange=record,
        tools_for=tools_for,
    )


class NativeMockProvider(Provider):
    """A provider that speaks a native dialect and records what it was sent."""

    def __init__(self, responses, dialect=ToolDialect.OPENAI):
        super().__init__(retries=0, backoff_sec=0)
        self.tool_dialect = dialect
        self._responses = list(responses)
        self.calls = []

    def chat(self, model, messages, tools=None):
        self.calls.append({"messages": messages, "tools": tools})
        return self._responses[min(len(self.calls) - 1, len(self._responses) - 1)]


class TestPlainTurn:
    def test_a_reply_without_tool_calls_is_returned_as_is(self):
        provider = SequenceMockProvider(responses=["My view."])
        assert runner(provider).run([], "turn") == "My view."

    def test_the_instruction_is_appended_after_the_history(self):
        provider = SequenceMockProvider(responses=["ok"])
        history = [{"role": "assistant", "content": "[Mod] @Alice, go."}]
        runner(provider).run(history, "turn", instruction="Speak now.")

        assert provider.calls[0]["messages"] == [
            {"role": "system", "content": "BASE"},
            {"role": "assistant", "content": "[Mod] @Alice, go."},
            {"role": "user", "content": "Speak now."},
        ]

    def test_the_caller_history_is_not_mutated(self):
        provider = SequenceMockProvider(responses=[call_block("ping"), "done"])
        history = [{"role": "user", "content": "topic"}]
        runner(provider, spec()).run(history, "turn", instruction="go")
        assert history == [{"role": "user", "content": "topic"}]


class TestToolLoop:
    def test_tool_output_is_fed_back_and_the_final_text_returned(self):
        """The instruction rides along into the follow-up. Rebuilding the
        follow-up from shared history alone would drop it, and the model would
        come back from its tool call not knowing what it was asked."""
        provider = SequenceMockProvider(responses=[call_block("ping"), "Got pong."])
        result = runner(provider, spec()).run(
            [], "orchestrator turn", instruction="Open the debate."
        )
        assert result == "Got pong."

        followup = provider.calls[1]["messages"]
        assert {"role": "assistant", "content": "[Tool:ping] pong"} in followup
        assert followup[-1]["content"] == "Tool results are available above. Continue."
        assert "Open the debate." in [m["content"] for m in followup]
        assert provider.calls[1]["purpose"] == "orchestrator turn (tool followup)"

    def test_the_loop_runs_more_than_one_round(self):
        """A model that reads a file and then runs a command based on what it
        found needs two rounds inside one turn."""
        provider = SequenceMockProvider(responses=[
            call_block("read"), call_block("ping"), "Done after two rounds.",
        ])
        result = runner(provider, spec(), spec("read", lambda a: "contents")).run(
            [], "turn"
        )
        assert result == "Done after two rounds."
        assert len(provider.calls) == 3

    def test_an_error_result_is_fed_back_rather_than_re_prompted(self):
        """A bad call is the model's own to correct on the next iteration."""
        provider = SequenceMockProvider(responses=[
            call_block("teleport"), "Sorry, I'll use ping instead.",
        ])
        result = runner(provider, spec()).run([], "turn")

        assert result == "Sorry, I'll use ping instead."
        fed_back = "\n".join(m["content"] for m in provider.calls[1]["messages"])
        assert "[ToolError] Unknown tool: teleport" in fed_back

    def test_a_model_stuck_on_invalid_json_does_not_loop_forever(self):
        """An invalid block returns the same text every time, so a model that
        cannot fix it makes no progress — and max_tool_iterations is None by
        default, so nothing else bounds the loop."""
        provider = SequenceMockProvider(responses=["```tool_calls\n{bad\n```"])
        result = runner(provider, spec()).run([], "turn")

        # Each bad response costs one call; the third trips the bound.
        assert len(provider.calls) == MAX_INVALID_CALLS
        assert result == "```tool_calls\n{bad\n```"

    def test_a_recovering_model_is_not_penalised_for_an_earlier_bad_block(self):
        """The counter tracks *consecutive* failures — one bad block followed
        by a good one must not count toward the next run of bad ones."""
        provider = SequenceMockProvider(responses=[
            "```tool_calls\n{bad\n```", call_block("ping"), "Recovered.",
        ])
        assert runner(provider, spec()).run([], "turn") == "Recovered."

    def test_max_tool_iterations_stops_the_loop(self):
        provider = SequenceMockProvider(responses=[call_block("ping")])
        result = runner(provider, spec(), max_tool_iterations=2).run([], "turn")

        # 1 opening call + 2 followups, then the bound stops it.
        assert len(provider.calls) == 3
        assert result == call_block("ping")


class TestToolExchangesArePrivate:
    def test_recording_captures_the_whole_exchange_when_asked(self):
        provider = SequenceMockProvider(responses=[call_block("ping"), "done"])
        recorded: list[dict] = []
        runner(provider, spec(), record=recorded.append).run([], "turn")

        assert [m["content"] for m in recorded] == [
            call_block("ping"),
            "[Tool:ping] pong",
            "Tool results are available above. Continue.",
        ]


class TestProviderFailure:
    def test_a_failed_opening_call_returns_a_placeholder(self):
        class Failing(Provider):
            def chat(self, model, messages):
                raise ProviderError("down")

            def chat_with_retries(self, model, messages, purpose=""):
                raise ProviderError("down")

        result = runner(Failing()).run([], "turn from Alice")
        assert result == "[No response from model for turn from Alice]"

    def test_a_failed_followup_returns_a_placeholder(self):
        class FailsAfterFirst(Provider):
            def __init__(self):
                super().__init__(retries=0, backoff_sec=0)
                self.n = 0

            def chat(self, model, messages):
                return self.chat_with_retries(model, messages)

            def chat_with_retries(self, model, messages, purpose=""):
                self.n += 1
                if self.n == 1:
                    return ProviderResponse(content=call_block("ping"), model=model)
                raise ProviderError("down")

        result = runner(FailsAfterFirst(), spec()).run([], "turn")
        assert result == "[No response from model for turn]"


class TestNativeDialect:
    def test_a_native_call_round_trips_through_the_response_not_the_text(self):
        """The follow-up replays the assistant turn and its result — both native
        APIs 400 without it — and adds no nudge of its own: a tool result is
        already a turn, which is what TEXT needs the extra prompt to fake."""
        provider = NativeMockProvider([
            ProviderResponse(
                content="",
                tool_calls=[ToolCall("ping", {}, id="c1")],
                stop_reason="tool_calls",
            ),
            ProviderResponse(content="Got pong."),
        ])
        result = runner(provider, spec(), tools_for=lambda: [spec()]).run([], "turn")
        assert result == "Got pong."

        followup = provider.calls[1]["messages"]
        assert followup[-2]["tool_calls"][0]["id"] == "c1"
        assert followup[-1] == {
            "role": "tool", "tool_call_id": "c1", "content": "pong"
        }
        assert not any(FOLLOWUP_PROMPT in str(m) for m in followup)

    def test_schemas_are_sent_natively_and_never_under_text(self):
        """Under TEXT the specs already reached the model in the prompt."""
        native = NativeMockProvider([ProviderResponse(content="hi")])
        runner(native, spec(), tools_for=lambda: [spec()]).run([], "turn")
        assert native.calls[0]["tools"] == [spec()]

        text = NativeMockProvider(
            [ProviderResponse(content="hi")], dialect=ToolDialect.TEXT
        )
        runner(text, spec(), tools_for=lambda: [spec()]).run([], "turn")
        assert text.calls[0]["tools"] is None

    def test_anthropic_results_ride_in_a_user_message(self):
        provider = NativeMockProvider([
            ProviderResponse(content="", tool_calls=[ToolCall("ping", {}, id="tu_1")]),
            ProviderResponse(content="done"),
        ], dialect=ToolDialect.ANTHROPIC)
        runner(provider, spec(), tools_for=lambda: [spec()]).run([], "turn")

        last = provider.calls[1]["messages"][-1]
        assert last["role"] == "user"
        assert last["content"][0]["tool_use_id"] == "tu_1"

    def test_a_fenced_call_still_works_under_a_native_dialect(self):
        """A natively-equipped model may still narrate a call in prose."""
        provider = NativeMockProvider([
            ProviderResponse(content=call_block("ping")),
            ProviderResponse(content="done"),
        ])
        result = runner(provider, spec(), tools_for=lambda: [spec()]).run([], "turn")
        assert result == "done"
