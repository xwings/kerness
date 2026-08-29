"""Tests for kerness.provider."""

import inspect
from typing import Literal
from unittest.mock import patch

import pytest
from pydantic import BaseModel

from kerness.exceptions import ProviderError, ProviderHTTPError
from kerness.provider import (
    CLAUDE_BASE_URL,
    DEFAULT_BACKOFF_SEC,
    DEFAULT_CLAUDE_MAX_TOKENS,
    DEFAULT_REQUEST_TIMEOUT_SEC,
    DEFAULT_RETRIES,
    DEFAULT_TEMPERATURE,
    DEFAULT_TOP_P,
    OPENAI_BASE_URL,
    OPENROUTER_BASE_URL,
    ClaudeOAuthProvider,
    ClaudeProvider,
    CustomProvider,
    OpenAIOAuthProvider,
    OpenAIProvider,
    OpenRouterProvider,
    Provider,
    ProviderResponse,
    _convert_messages_for_claude,
)
from kerness.tooling import ToolCall, ToolSpec
from kerness.toolschema import ToolDialect


def _reply(content="reply", model="gpt-4o", usage=None):
    """The OpenAI-shaped envelope every chat-completions mock has to return."""
    return {
        "choices": [{"message": {"content": content}}],
        "model": model,
        "usage": usage or {},
    }


class StructuredAnswer(BaseModel):
    answer: str
    score: int


class OptionalStructuredAnswer(BaseModel):
    answer: str
    note: str | None = None


class TileAction(BaseModel):
    action: Literal["discard", "chi", "pon", "ron", "pass"]
    tile: str
    consume_tiles: list[str]


class TurnMeta(BaseModel):
    round_idx: int
    wall_remaining: int


class ComplexTurnDecision(BaseModel):
    player: str
    legal_actions: list[TileAction]
    chosen: TileAction | None = None
    meta: TurnMeta


class TestProviderResponse:
    def test_every_field_is_carried_or_defaulted(self):
        """Only ``content`` is required; a mutable default that leaked between
        responses would make one turn's usage show up on the next."""
        resp = ProviderResponse(
            content="hello",
            model="test/model",
            usage={"prompt_tokens": 10, "completion_tokens": 5},
            raw={"id": "123"},
        )
        assert resp.content == "hello"
        assert resp.model == "test/model"
        assert resp.usage["prompt_tokens"] == 10
        assert resp.raw["id"] == "123"

        bare = ProviderResponse(content="hi")
        assert bare.model == ""
        assert bare.usage == {}
        assert bare.raw == {}
        assert bare.structured is None


class TestOpenRouterChat:
    @patch("kerness.provider.http_post_json")
    def test_one_call_out_and_back(self, mock_post):
        """The attribution headers are what OpenRouter ranks apps by, and the
        model that comes back is the one it actually routed to, not the one
        that was asked for."""
        mock_post.return_value = _reply("  parsed reply  ", model="actual/model",
                                        usage={"prompt_tokens": 5})
        provider = OpenRouterProvider(
            api_key="sk-test", app_name="TestApp", app_url="http://test.com"
        )
        resp = provider.chat("test/model", [{"role": "user", "content": "hi"}])

        url, payload, headers = mock_post.call_args[0][:3]
        assert "chat/completions" in url
        assert headers["Authorization"] == "Bearer sk-test"
        assert headers["X-Title"] == "TestApp"
        assert headers["HTTP-Referer"] == "http://test.com"
        assert payload["model"] == "test/model"
        assert payload["messages"] == [{"role": "user", "content": "hi"}]

        assert resp.content == "parsed reply"
        assert resp.model == "actual/model"
        assert resp.usage == {"prompt_tokens": 5}


NESTED_JSON = (
    '{"player":"South","legal_actions":[{"action":"chi","tile":"3p",'
    '"consume_tiles":["2p","4p"]}],"chosen":{"action":"chi","tile":"3p",'
    '"consume_tiles":["2p","4p"]},"meta":{"round_idx":3,"wall_remaining":54}}'
)


class TestOpenAIChat:
    @patch("kerness.provider.http_post_json")
    def test_a_plain_turn_is_untouched(self, mock_post):
        """No `output_type` means no `response_format` on the way out and no
        `structured` on the way back — the payload is what it was before
        structured output existed."""
        mock_post.return_value = _reply("  openai reply  ", usage={"prompt_tokens": 5})
        resp = OpenAIProvider(api_key="sk-test").chat(
            "gpt-4o", [{"role": "user", "content": "hi"}]
        )

        url, payload, headers = mock_post.call_args[0][:3]
        assert "api.openai.com" in url
        assert "chat/completions" in url
        assert headers["Authorization"] == "Bearer sk-test"
        assert payload["model"] == "gpt-4o"
        assert "response_format" not in payload

        assert resp.content == "openai reply"
        assert resp.structured is None

    @patch("kerness.provider.http_post_json")
    def test_openai_structured_builds_response_format_payload(self, mock_post):
        mock_post.return_value = _reply('{"answer":"yes","score":9}')
        provider = OpenAIProvider(
            api_key="sk-test",
            output_type=StructuredAnswer,
            strict_json_schema=True,
            output_schema_name="my_schema",
        )
        provider.chat("gpt-4o", [{"role": "user", "content": "hi"}])

        fmt = mock_post.call_args[0][1]["response_format"]
        assert fmt["type"] == "json_schema"
        assert fmt["json_schema"]["name"] == "my_schema"
        assert fmt["json_schema"]["strict"] is True
        schema = fmt["json_schema"]["schema"]
        assert schema["type"] == "object"
        assert schema["additionalProperties"] is False
        assert sorted(schema["required"]) == ["answer", "score"]

        # With no explicit name the output type supplies one.
        OpenAIProvider(api_key="sk-test", output_type=StructuredAnswer).chat(
            "gpt-4o", [{"role": "user", "content": "hi"}]
        )
        named = mock_post.call_args[0][1]["response_format"]["json_schema"]
        assert named["name"] == "StructuredAnswer"

    @patch("kerness.provider.http_post_json")
    def test_strict_mode_is_what_makes_an_optional_field_required(self, mock_post):
        """OpenAI's strict schemas have no notion of optional, so the rewriter
        promotes every field to required and drops the default. Non-strict
        passes the model's own schema through."""
        mock_post.return_value = _reply('{"answer":"yes","note":null}')

        for strict in (False, True):
            OpenAIProvider(
                api_key="sk-test",
                output_type=OptionalStructuredAnswer,
                strict_json_schema=strict,
            ).chat("gpt-4o", [{"role": "user", "content": "hi"}])

            fmt = mock_post.call_args[0][1]["response_format"]["json_schema"]
            schema = fmt["schema"]
            assert fmt["strict"] is strict
            if strict:
                assert sorted(schema["required"]) == ["answer", "note"]
                assert schema["additionalProperties"] is False
                assert "default" not in schema["properties"]["note"]
            else:
                assert schema["required"] == ["answer"]
                assert "additionalProperties" not in schema
                assert schema["properties"]["note"]["default"] is None

    @patch("kerness.provider.http_post_json")
    def test_the_strict_rewrite_reaches_nested_models_too(self, mock_post):
        """A rewriter that only walks the top level leaves `$defs` permissive,
        and OpenAI rejects the whole schema for it."""
        mock_post.return_value = _reply(NESTED_JSON)
        OpenAIProvider(
            api_key="sk-test",
            output_type=ComplexTurnDecision,
            strict_json_schema=True,
        ).chat("gpt-4o", [{"role": "user", "content": "hi"}])

        fmt = mock_post.call_args[0][1]["response_format"]["json_schema"]
        schema = fmt["schema"]

        assert fmt["strict"] is True
        assert schema["additionalProperties"] is False
        assert sorted(schema["required"]) == ["chosen", "legal_actions", "meta", "player"]
        assert set(schema["$defs"].keys()) == {"TileAction", "TurnMeta"}
        assert schema["$defs"]["TileAction"]["additionalProperties"] is False
        assert schema["$defs"]["TurnMeta"]["additionalProperties"] is False
        assert schema["properties"]["legal_actions"]["items"]["$ref"] == "#/$defs/TileAction"
        assert schema["properties"]["chosen"]["anyOf"][0]["$ref"] == "#/$defs/TileAction"

    @patch("kerness.provider.http_post_json")
    def test_a_valid_reply_is_parsed_at_any_depth(self, mock_post):
        """The content stays the raw JSON so a caller who wants the text still
        has it; ``structured`` is the parsed model beside it."""
        mock_post.return_value = _reply(
            '  {"answer":"ok","score":7}  ', usage={"prompt_tokens": 5}
        )
        flat = OpenAIProvider(
            api_key="sk-test", output_type=StructuredAnswer
        ).chat("gpt-4o", [])
        assert flat.content == '{"answer":"ok","score":7}'
        assert isinstance(flat.structured, StructuredAnswer)
        assert flat.structured.answer == "ok"
        assert flat.structured.score == 7

        mock_post.return_value = _reply(NESTED_JSON)
        nested = OpenAIProvider(
            api_key="sk-test", output_type=ComplexTurnDecision
        ).chat("gpt-4o", [])
        assert isinstance(nested.structured, ComplexTurnDecision)
        assert nested.structured.player == "South"
        assert nested.structured.meta.round_idx == 3
        assert nested.structured.chosen is not None
        assert nested.structured.chosen.action == "chi"
        assert nested.structured.legal_actions[0].consume_tiles == ["2p", "4p"]

    @pytest.mark.parametrize("output_type, content", [
        # Missing a required top-level field.
        (StructuredAnswer, '{"answer":"oops"}'),
        # Missing a required field inside a nested model.
        (ComplexTurnDecision,
         '{"player":"South","legal_actions":[{"action":"chi","tile":"3p"}],'
         '"chosen":null,"meta":{"round_idx":3,"wall_remaining":54}}'),
    ])
    @patch("kerness.provider.http_post_json")
    def test_a_reply_that_does_not_validate_raises(
        self, mock_post, output_type, content
    ):
        mock_post.return_value = _reply(content)
        provider = OpenAIProvider(api_key="sk-test", output_type=output_type)
        with pytest.raises(ProviderError, match="Structured output parsing failed"):
            provider.chat("gpt-4o", [])


class TestOpenAIOAuthChat:
    @patch("kerness.provider.http_post_json")
    def test_it_swaps_the_credential_and_keeps_everything_else(self, mock_post):
        """Only the header differs from the API-key provider; structured output
        inheriting from it is the whole reason it subclasses rather than
        reimplements."""
        mock_post.return_value = _reply('{"answer":"oauth","score":10}')
        resp = OpenAIOAuthProvider(
            oauth_token="oauth-token-123", output_type=StructuredAnswer
        ).chat("gpt-4o", [{"role": "user", "content": "hi"}])

        payload, headers = mock_post.call_args[0][1:3]
        assert headers["Authorization"] == "Bearer oauth-token-123"
        assert "api_key" not in headers
        assert payload["response_format"]["type"] == "json_schema"
        assert isinstance(resp.structured, StructuredAnswer)
        assert resp.structured.answer == "oauth"


class TestClaudeChat:
    @patch("kerness.provider.http_post_json")
    def test_one_call_out_and_back(self, mock_post):
        """Anthropic takes the system prompt as its own field; leaving it in
        the messages list is a 400."""
        mock_post.return_value = {
            "content": [{"text": "  claude reply  "}],
            "model": "claude-sonnet-4-20250514",
            "usage": {"input_tokens": 10},
        }
        resp = ClaudeProvider(api_key="sk-ant-test").chat(
            "claude-sonnet-4-20250514",
            [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"},
            ],
        )

        url, payload, headers = mock_post.call_args[0][:3]
        assert "api.anthropic.com" in url
        assert "/messages" in url
        assert headers["x-api-key"] == "sk-ant-test"
        assert headers["anthropic-version"] == "2023-06-01"
        assert payload["system"] == "You are helpful."
        assert all(m["role"] != "system" for m in payload["messages"])

        assert resp.content == "claude reply"


class TestClaudeOAuthChat:
    @patch("kerness.provider.http_post_json")
    def test_uses_oauth_bearer(self, mock_post):
        mock_post.return_value = {
            "content": [{"text": "reply"}],
            "model": "claude-sonnet-4-20250514",
            "usage": {},
        }
        provider = ClaudeOAuthProvider(oauth_token="oauth-token-456")
        provider.chat("claude-sonnet-4-20250514", [{"role": "user", "content": "hi"}])

        args, kwargs = mock_post.call_args
        headers = args[2]
        assert headers["Authorization"] == "Bearer oauth-token-456"
        assert "x-api-key" not in headers
        assert headers["anthropic-version"] == "2023-06-01"


class TestConvertMessagesForClaude:
    def test_every_system_message_is_lifted_out_and_none_is_lost(self):
        """Taking only the first would silently drop a second one — and a
        conversation with no system message at all is ordinary, not an error."""
        system, filtered = _convert_messages_for_claude([
            {"role": "system", "content": "Be helpful."},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
        ])
        assert system == "Be helpful."
        assert len(filtered) == 2
        assert all(m["role"] != "system" for m in filtered)

        system, filtered = _convert_messages_for_claude([
            {"role": "system", "content": "First."},
            {"role": "system", "content": "Second."},
            {"role": "user", "content": "hi"},
        ])
        assert "First." in system
        assert "Second." in system
        assert len(filtered) == 1

        system, filtered = _convert_messages_for_claude(
            [{"role": "user", "content": "hi"}]
        )
        assert system == ""
        assert len(filtered) == 1


class TestChatWithRetries:
    @patch("kerness.provider.http_post_json")
    def test_the_budget_is_spent_only_on_failure_and_then_reported(
        self, mock_post
    ):
        """Retrying a call that already succeeded would double every bill, and
        a budget that ran out silently would return nothing to a caller
        expecting a turn."""
        mock_post.return_value = _reply("ok", model="m")
        provider = OpenRouterProvider(api_key="sk-test", retries=2, backoff_sec=0)
        assert provider.chat_with_retries("m", [], purpose="test").content == "ok"
        assert mock_post.call_count == 1

        mock_post.reset_mock()
        mock_post.return_value = None
        mock_post.side_effect = [Exception("fail"), _reply("ok", model="m")]
        assert provider.chat_with_retries("m", [], purpose="test").content == "ok"
        assert mock_post.call_count == 2

        mock_post.side_effect = Exception("always fail")
        with pytest.raises(ProviderError, match="All retries exhausted"):
            provider.chat_with_retries("m", [], purpose="test")


class TestCustomProvider:
    @patch("kerness.provider.http_post_json")
    def test_one_call_out_and_back(self, mock_post):
        """The whole round trip: a trailing slash on the base URL does not
        double up, the key becomes a bearer header, the payload is not
        streaming and carries no `max_tokens` unless configured, and the reply
        comes back stripped with the model the server actually used."""
        mock_post.return_value = _reply(
            "  custom reply  ", model="actual-model", usage={"prompt_tokens": 10}
        )
        provider = CustomProvider(
            url="https://coding.dashscope.aliyuncs.com/v1/",
            api_key="sk-custom",
        )
        resp = provider.chat("qwen3.5-plus", [{"role": "user", "content": "hi"}])

        url, payload, headers = mock_post.call_args[0][:3]
        assert url == "https://coding.dashscope.aliyuncs.com/v1/chat/completions"
        assert headers["Authorization"] == "Bearer sk-custom"
        assert payload["model"] == "qwen3.5-plus"
        assert payload["stream"] is False
        assert "max_tokens" not in payload

        assert resp.content == "custom reply"
        assert resp.model == "actual-model"
        assert resp.usage == {"prompt_tokens": 10}

    @pytest.mark.parametrize("kwargs, expected", [
        ({"model_config": {"id": "qwen3.5-plus", "maxTokens": 65536}}, 65536),
        # An explicit argument outranks whatever the model config declares.
        ({"model_config": {"maxTokens": 65536}, "max_tokens": 4096}, 4096),
    ])
    @patch("kerness.provider.http_post_json")
    def test_max_tokens_source(self, mock_post, kwargs, expected):
        mock_post.return_value = _reply(model="m")
        CustomProvider(
            url="https://example.com/v1", api_key="sk-test", **kwargs
        ).chat("m", [])

        assert mock_post.call_args[0][1]["max_tokens"] == expected

    @patch("kerness.provider.http_post_json")
    def test_extra_headers_and_body_are_merged_not_replaced(self, mock_post):
        mock_post.return_value = _reply(model="m")
        CustomProvider(
            url="https://example.com/v1",
            api_key="sk-test",
            extra_headers={"X-Custom": "value"},
            extra_body={"enable_search": True, "top_k": 50},
        ).chat("m", [])

        payload, headers = mock_post.call_args[0][1:3]
        assert headers["X-Custom"] == "value"
        assert headers["Authorization"] == "Bearer sk-test"
        assert payload["enable_search"] is True
        assert payload["top_k"] == 50

    def test_model_config_property_returns_copy(self):
        cfg = {"id": "qwen3.5-plus", "maxTokens": 65536}
        provider = CustomProvider(
            url="https://example.com/v1", api_key="sk-test", model_config=cfg
        )
        returned = provider.model_config
        assert returned == cfg
        returned["id"] = "mutated"
        assert provider.model_config["id"] == "qwen3.5-plus"


class TestAnUnreadableEnvelope:
    @patch("kerness.provider.http_post_json")
    def test_every_provider_refuses_a_body_it_cannot_read(self, mock_post):
        """One contract across three transports: a body that is not the shape
        the API documents is an error, not a `ProviderResponse` carrying junk
        that a turn then puts in front of a model."""
        mock_post.return_value = {"error": "bad request"}
        for provider in (
            OpenRouterProvider(api_key="sk-test"),
            ClaudeProvider(api_key="sk-ant-test"),
            CustomProvider(url="https://example.com/v1", api_key="sk-test"),
        ):
            with pytest.raises(ProviderError, match="Unexpected response"):
                provider.chat("m", [{"role": "user", "content": "hi"}])


CMD_SPEC = ToolSpec(
    name="cmd",
    description="Run a shell command.",
    parameters={"type": "object", "properties": {"command": {"type": "string"}}},
    handler=lambda args: "",
)


class TestDialectDetection:
    """Three tiers, in order, with no sniffing of a successful response."""

    def test_declared_dialect_wins_when_chat_can_carry_tools(self):
        assert OpenAIProvider(api_key="k").effective_dialect() is ToolDialect.OPENAI
        assert ClaudeProvider(api_key="k").effective_dialect() is ToolDialect.ANTHROPIC

    def test_a_chat_that_cannot_carry_tools_falls_back_to_text(self):
        """This is what keeps hand-written test doubles working untouched —
        and ``**kwargs`` has to count, or every forwarding wrapper degrades."""
        class ToolsUnaware(Provider):
            tool_dialect = ToolDialect.OPENAI

            def chat(self, model, messages):
                return ProviderResponse(content="hi", model=model)

        class Flexible(Provider):
            tool_dialect = ToolDialect.OPENAI

            def chat(self, model, messages, **kwargs):
                return ProviderResponse(content="hi", model=model)

        assert ToolsUnaware().effective_dialect() is ToolDialect.TEXT
        assert Flexible().effective_dialect() is ToolDialect.OPENAI

    def test_a_400_naming_tools_latches_down_to_text_for_good(self):
        """Flipping back would put two dialects in one conversation."""
        provider = OpenAIProvider(api_key="k")
        exc = ProviderHTTPError(400, "https://x", "tools is not supported")

        assert provider.note_native_tools_rejected(exc) is True
        assert provider.effective_dialect() is ToolDialect.TEXT
        assert provider.effective_dialect() is ToolDialect.TEXT

    def test_a_failure_that_is_not_about_tools_does_not_latch(self):
        """A bad key or a server fault says nothing about tool support, and
        degrading over one costs native calling for the rest of the run."""
        provider = OpenAIProvider(api_key="k")

        assert provider.note_native_tools_rejected(
            ProviderHTTPError(400, "https://x", "invalid api key")
        ) is False
        assert provider.note_native_tools_rejected(
            ProviderHTTPError(500, "https://x", "tools broke")
        ) is False
        assert provider.effective_dialect() is ToolDialect.OPENAI


class TestNativeToolCalling:
    @patch("kerness.provider.http_post_json")
    def test_each_dialect_sends_its_own_schema_shape(self, mock_post):
        mock_post.return_value = _reply("ok", model="m")
        OpenAIProvider(api_key="k").chat("m", [], tools=[CMD_SPEC])
        assert mock_post.call_args[0][1]["tools"][0]["function"]["name"] == "cmd"

        mock_post.return_value = {"content": [{"text": "ok"}], "model": "m"}
        ClaudeProvider(api_key="k").chat("m", [], tools=[CMD_SPEC])
        assert mock_post.call_args[0][1]["tools"][0]["input_schema"] == (
            CMD_SPEC.parameters
        )

    @patch("kerness.provider.http_post_json")
    def test_no_tools_key_when_there_is_nothing_to_send(self, mock_post):
        """An empty ``tools: []`` is a 400 at OpenAI, and a latched provider
        offering schemas again would earn the same 400 every turn."""
        mock_post.return_value = _reply("ok", model="m")
        OpenAIProvider(api_key="k").chat("m", [])
        assert "tools" not in mock_post.call_args[0][1]

        latched = OpenAIProvider(api_key="k")
        latched.note_native_tools_rejected(
            ProviderHTTPError(400, "https://x", "tools unsupported")
        )
        latched.chat("m", [], tools=[CMD_SPEC])
        assert "tools" not in mock_post.call_args[0][1]

    @patch("kerness.provider.http_post_json")
    def test_openai_parses_a_tool_call_with_null_content(self, mock_post):
        mock_post.return_value = {
            "choices": [{
                "message": {
                    "content": None,
                    "tool_calls": [{
                        "id": "c1", "type": "function",
                        "function": {"name": "cmd", "arguments": '{"command":"ls"}'},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "model": "m",
        }
        resp = OpenAIProvider(api_key="k").chat("m", [])
        assert resp.content == ""
        assert resp.tool_calls == [ToolCall("cmd", {"command": "ls"}, id="c1")]
        assert resp.stop_reason == "tool_calls"

    @patch("kerness.provider.http_post_json")
    def test_claude_reads_every_block_in_the_content_list(self, mock_post):
        """Anthropic returns a list; taking only ``content[0]`` drops the
        second half of a two-paragraph reply and misses a tool_use block
        that follows a text one."""
        mock_post.return_value = {
            "content": [{"type": "tool_use", "id": "tu_1", "name": "cmd",
                         "input": {"command": "ls"}}],
            "model": "m",
            "stop_reason": "tool_use",
        }
        resp = ClaudeProvider(api_key="k").chat("m", [])
        assert resp.content == ""
        assert resp.tool_calls == [ToolCall("cmd", {"command": "ls"}, id="tu_1")]
        assert resp.stop_reason == "tool_use"

        mock_post.return_value = {
            "content": [{"type": "text", "text": "one"},
                        {"type": "text", "text": "two"}],
            "model": "m",
        }
        assert ClaudeProvider(api_key="k").chat("m", []).content == "one\ntwo"

    @patch("kerness.provider.http_post_json")
    def test_structured_output_is_skipped_on_a_tool_calling_turn(self, mock_post):
        """There is no JSON answer yet — it comes after the results go back."""
        mock_post.return_value = {
            "choices": [{"message": {
                "content": None,
                "tool_calls": [{"id": "c1", "function": {
                    "name": "cmd", "arguments": "{}"}}],
            }}],
            "model": "m",
        }
        provider = OpenAIProvider(api_key="k", output_type=StructuredAnswer)
        resp = provider.chat("m", [])
        assert resp.structured is None
        assert resp.tool_calls


class TestEmptyResponseGuard:
    """A native tool-use turn legitimately has no text."""

    @patch("kerness.provider.http_post_json")
    def test_it_is_the_tool_call_and_not_the_text_that_makes_a_turn_real(
        self, mock_post
    ):
        """Empty content beside a tool call is the normal shape of a tool-use
        turn and must not be retried; empty content beside nothing at all is
        still the failure the guard exists for."""
        mock_post.return_value = {
            "content": [{"type": "tool_use", "id": "tu_1", "name": "cmd",
                         "input": {}}],
            "model": "m",
        }
        provider = ClaudeProvider(api_key="k", retries=0, backoff_sec=0)
        resp = provider.chat_with_retries("m", [], purpose="turn", tools=[CMD_SPEC])
        assert resp.content == ""
        assert mock_post.call_count == 1

        mock_post.return_value = {
            "choices": [{"message": {"content": "   "}}], "model": "m",
        }
        with pytest.raises(ProviderError):
            OpenAIProvider(
                api_key="k", retries=0, backoff_sec=0
            ).chat_with_retries("m", [], purpose="turn")

    @patch("kerness.provider.http_post_json")
    def test_a_rejected_endpoint_retries_once_without_tools(self, mock_post):
        """The degrade latch salvages the turn instead of failing it."""
        mock_post.side_effect = [
            ProviderHTTPError(400, "https://x", "tools is not supported"),
            {"choices": [{"message": {"content": "plain reply"}}], "model": "m"},
        ]
        provider = CustomProvider(url="https://x/v1", api_key="k",
                                  retries=0, backoff_sec=0)
        resp = provider.chat_with_retries("m", [], purpose="turn", tools=[CMD_SPEC])
        assert resp.content == "plain reply"
        assert "tools" not in mock_post.call_args[0][1]


class TestReasoningEffort:
    """The level is per agent, so it travels per call rather than per provider."""

    @patch("kerness.provider.http_post_json")
    def test_each_backend_spells_the_level_its_own_way(self, mock_post):
        """There is no shared key, and nothing normalises between them."""
        mock_post.return_value = _reply("ok", model="m")
        OpenAIProvider(api_key="k").chat("m", [], reasoning_effort="low")
        assert mock_post.call_args[0][1]["reasoning_effort"] == "low"

        CustomProvider(url="https://x/v1", api_key="k").chat(
            "m", [], reasoning_effort="xhigh"
        )
        assert mock_post.call_args[0][1]["reasoning_effort"] == "xhigh"

        OpenRouterProvider(api_key="k").chat("m", [], reasoning_effort="minimal")
        assert mock_post.call_args[0][1]["reasoning"] == {"effort": "minimal"}

        mock_post.return_value = {"content": [{"text": "ok"}], "model": "m"}
        ClaudeProvider(api_key="k").chat("m", [], reasoning_effort="max")
        assert mock_post.call_args[0][1]["output_config"] == {"effort": "max"}

    @patch("kerness.provider.http_post_json")
    def test_the_level_is_high_when_nobody_names_one(self, mock_post):
        """``high`` is sent rather than standing in for "unset"."""
        mock_post.return_value = _reply("ok", model="m")
        OpenAIProvider(api_key="k").chat("m", [])
        assert mock_post.call_args[0][1]["reasoning_effort"] == "high"

    def test_an_unknown_level_is_rejected_where_it_was_written(self):
        provider = OpenAIProvider(api_key="k")
        with pytest.raises(ValueError, match="Unknown reasoning effort"):
            provider.chat("m", [], reasoning_effort="thorough")

    def test_a_400_naming_the_parameter_latches_it_off_for_good(self):
        provider = OpenAIProvider(api_key="k")
        exc = ProviderHTTPError(400, "https://x", "unknown parameter reasoning_effort")

        assert provider.note_reasoning_effort_rejected(exc) is True
        assert provider.effective_effort("high") is None
        assert provider.effective_effort("low") is None

    def test_a_second_refusal_is_reported_rather_than_retried(self):
        """The retry re-sends identical arguments, so a latch that reported
        itself twice would never stop."""
        provider = OpenAIProvider(api_key="k")
        exc = ProviderHTTPError(400, "https://x", "unsupported: reasoning_effort")

        assert provider.note_reasoning_effort_rejected(exc) is True
        assert provider.note_reasoning_effort_rejected(exc) is False

    def test_a_failure_that_is_not_about_the_level_does_not_latch(self):
        provider = OpenAIProvider(api_key="k")

        assert provider.note_reasoning_effort_rejected(
            ProviderHTTPError(400, "https://x", "invalid api key")
        ) is False
        assert provider.note_reasoning_effort_rejected(
            ProviderHTTPError(500, "https://x", "reasoning_effort broke")
        ) is False
        assert provider.effective_effort("high") == "high"

    @patch("kerness.provider.http_post_json")
    def test_a_rejected_endpoint_retries_once_without_the_level(self, mock_post):
        mock_post.side_effect = [
            ProviderHTTPError(400, "https://x", "unrecognised key reasoning_effort"),
            {"choices": [{"message": {"content": "plain reply"}}], "model": "m"},
        ]
        provider = CustomProvider(url="https://x/v1", api_key="k",
                                  retries=0, backoff_sec=0)
        resp = provider.chat_with_retries("m", [], purpose="turn")
        assert resp.content == "plain reply"
        assert "reasoning_effort" not in mock_post.call_args[0][1]

    def test_a_chat_that_never_declared_the_level_is_never_offered_one(self):
        """The same courtesy ``tools`` gets: a provider written before the
        parameter existed keeps working untouched."""
        seen = []

        class EffortUnaware(Provider):
            def chat(self, model, messages, tools=None):
                seen.append("no kwarg")
                return ProviderResponse(content="hi", model=model)

        class Aware(Provider):
            def chat(self, model, messages, tools=None, reasoning_effort=None):
                seen.append(reasoning_effort)
                return ProviderResponse(content="hi", model=model)

        EffortUnaware().chat_with_retries("m", [], purpose="turn")
        Aware().chat_with_retries("m", [], purpose="turn", reasoning_effort="low")
        assert seen == ["no kwarg", "low"]


class TestSharedDefaults:
    """The request defaults are declared once, in Rust, and named on both sides.

    Each built-in constructor writes these numbers into its own signature, so
    without this the wheel could ship a timeout the crate does not use and
    nothing would say so.
    """

    def test_the_constants_carry_the_frameworks_values(self):
        assert DEFAULT_REQUEST_TIMEOUT_SEC == 60
        assert DEFAULT_RETRIES == 2
        assert DEFAULT_BACKOFF_SEC == 2.0
        assert DEFAULT_TEMPERATURE == 1.0
        assert DEFAULT_TOP_P == 1.0
        assert DEFAULT_CLAUDE_MAX_TOKENS == 4096
        assert OPENAI_BASE_URL == "https://api.openai.com/v1"
        assert OPENROUTER_BASE_URL == "https://openrouter.ai/api/v1"
        assert CLAUDE_BASE_URL == "https://api.anthropic.com/v1"

    @pytest.mark.parametrize(
        ("cls", "base_url"),
        [
            (OpenAIProvider, OPENAI_BASE_URL),
            (OpenRouterProvider, OPENROUTER_BASE_URL),
            (ClaudeProvider, CLAUDE_BASE_URL),
        ],
    )
    def test_every_constructor_defaults_to_the_constants(self, cls, base_url):
        signature = inspect.signature(cls.__init__).parameters
        assert signature["base_url"].default == base_url
        assert signature["timeout_sec"].default == DEFAULT_REQUEST_TIMEOUT_SEC
        assert signature["retries"].default == DEFAULT_RETRIES
        assert signature["backoff_sec"].default == DEFAULT_BACKOFF_SEC
        assert signature["temperature"].default == DEFAULT_TEMPERATURE

    def test_the_two_defaults_only_one_family_takes(self):
        """``top_p`` is chat-completions only, and the reply ceiling is a
        constant on exactly one backend: Anthropic requires the field, so there
        is no "unset" to send, while OpenAI omits it and lets the model decide.
        """
        openai = inspect.signature(OpenAIProvider.__init__).parameters
        assert openai["top_p"].default == DEFAULT_TOP_P
        assert openai["max_tokens"].default is None

        claude = inspect.signature(ClaudeProvider.__init__).parameters
        assert claude["max_tokens"].default == DEFAULT_CLAUDE_MAX_TOKENS
        assert "top_p" not in claude
