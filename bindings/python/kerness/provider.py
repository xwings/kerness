"""LLM provider abstraction.

`Provider` is a Python abstract base class because that is what callers
subclass and what ``isinstance`` has to agree with. Everything it decides is
Rust: each instance owns a core, and the base class's methods hand ``self``
back down so the retry budget, the empty-reply guard, the one-way degrade
latch, and the three-tier dialect choice all run there while still calling
back out to whichever ``chat`` the subclass actually defined.

``http_post_json`` is re-exported here rather than only in :mod:`kerness.utils`
because this is the name the built-in backends resolve at call time — patching
it intercepts every request without any provider knowing there was something
to intercept.
"""

from __future__ import annotations

import inspect
from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any

from kerness._core import (
    ProviderCore,
    ProviderResponse,
    _convert_messages_for_claude,
    http_post_json,
)
from kerness._enums import ToolDialect
from kerness.exceptions import ProviderError

if TYPE_CHECKING:  # pragma: no cover - typing only
    from kerness.tooling import ToolSpec

__all__ = [
    "ClaudeOAuthProvider",
    "ClaudeProvider",
    "CustomProvider",
    "OpenAIOAuthProvider",
    "OpenAIProvider",
    "OpenRouterProvider",
    "Provider",
    "ProviderResponse",
    "_convert_messages_for_claude",
    "http_post_json",
]


def _require_pydantic() -> tuple[Any, Any]:
    """Import pydantic on demand.

    Structured output is the only feature that needs it, so the dependency is
    optional and the error names the extra that supplies it.
    """
    try:
        from pydantic import TypeAdapter, ValidationError
    except ModuleNotFoundError as exc:  # pragma: no cover - env dependent
        raise ImportError(
            "Structured output requires pydantic. "
            "Install it with: pip install 'kerness[structured]'"
        ) from exc
    return TypeAdapter, ValidationError


class Provider(ABC):
    """Abstract base class for LLM providers."""

    #: Dialect this provider speaks natively.  Subclasses override.
    tool_dialect: ToolDialect = ToolDialect.TEXT

    def __init__(self, retries: int = 2, backoff_sec: float = 2.0,
                 interval_sec: float | None = None) -> None:
        self._core = ProviderCore(retries, backoff_sec, interval_sec)

    @abstractmethod
    def chat(self, model: str, messages: list[dict[str, str]]) -> ProviderResponse:
        """Send a chat completion request.

        Args:
            model: Model identifier.
            messages: List of message dicts with 'role' and 'content'.

        Returns:
            ProviderResponse with the model's reply.
        """

    def effective_dialect(self) -> ToolDialect:
        """Return the dialect to actually use for this provider.

        Three tiers, checked in order and never sniffing a successful
        response body:

        1. A one-way degrade latch set by :meth:`note_native_tools_rejected`
           after the endpoint returns an HTTP error naming tool support.
        2. The declared :attr:`tool_dialect` class attribute.
        3. A capability probe — a subclass whose ``chat`` does not accept a
           ``tools`` argument cannot speak a native dialect regardless of what
           it declares.  This is what keeps hand-written test doubles and
           third-party subclasses working untouched.
        """
        return self._core.effective_dialect(self)

    def _chat_accepts_tools(self) -> bool:
        """Whether this subclass's ``chat`` can be offered tools.

        The one genuinely introspective step in the dialect choice, and the
        reason it stays Python: the answer is a property of the concrete
        class's signature, which only ``inspect`` can read.
        """
        try:
            params = inspect.signature(type(self).chat).parameters
        except (TypeError, ValueError):  # pragma: no cover - exotic callables
            return False
        if "tools" in params:
            return True
        return any(p.kind is inspect.Parameter.VAR_KEYWORD for p in params.values())

    def note_native_tools_rejected(self, exc: Exception) -> bool:
        """Latch this provider down to TEXT if *exc* means "no tool support".

        One-way: once dropped, the provider never re-attempts native calling
        for the rest of its life.  Returns True when the latch fired.
        """
        return self._core.note_native_tools_rejected(self, exc)

    def chat_with_retries(self, model: str, messages: list[dict[str, str]],
                          purpose: str = "",
                          tools: list["ToolSpec"] | None = None) -> ProviderResponse:
        """Send a chat request with automatic retries.

        Args:
            model: Model identifier.
            messages: List of message dicts.
            purpose: Human-readable description for logging.
            tools: Tool specs to advertise natively.  Ignored by providers
                whose effective dialect is ``TEXT``.

        Returns:
            ProviderResponse on success.

        Raises:
            ProviderError: If all retries are exhausted.
        """
        return self._core.chat_with_retries(self, model, messages, purpose, tools)

    def _chat_dispatch(self, model: str, messages: list[dict[str, str]],
                       tools: list["ToolSpec"] | None) -> ProviderResponse:
        """Call ``chat``, passing ``tools`` only when it can be used."""
        return self._core.chat_dispatch(self, model, messages, tools)


class OpenRouterProvider(Provider):
    """OpenRouter API provider."""

    DEFAULT_BASE_URL = "https://openrouter.ai/api/v1"
    tool_dialect = ToolDialect.OPENAI

    def __init__(
        self,
        api_key: str,
        base_url: str = DEFAULT_BASE_URL,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        top_p: float = 1.0,
        max_tokens: int | None = None,
        app_url: str = "",
        app_name: str = "",
    ) -> None:
        self._core = ProviderCore.openrouter(
            api_key,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            top_p,
            max_tokens,
            app_url,
            app_name,
        )

    def chat(self, model: str, messages: list[dict[str, str]],
             tools: list["ToolSpec"] | None = None) -> ProviderResponse:
        """Send a chat completion request to OpenRouter."""
        return self._core.chat(model, messages, tools)


class OpenAIProvider(Provider):
    """OpenAI API provider.

    ``output_type`` is the one place a Python object outlives the boundary: a
    pydantic model is validated here, on the reply the Rust backend already
    checked was JSON, so the caller gets the model instance rather than a dict.
    """

    DEFAULT_BASE_URL = "https://api.openai.com/v1"
    tool_dialect = ToolDialect.OPENAI

    def __init__(
        self,
        api_key: str,
        base_url: str = DEFAULT_BASE_URL,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        top_p: float = 1.0,
        max_tokens: int | None = None,
        output_type: type[Any] | None = None,
        strict_json_schema: bool = True,
        output_schema_name: str | None = None,
    ) -> None:
        self._output_type_adapter: Any | None = None
        self._validation_error: type[Exception] = ValueError
        schema: dict[str, Any] | None = None
        if output_type is not None:
            type_adapter_cls, validation_error = _require_pydantic()
            self._validation_error = validation_error
            self._output_type_adapter = type_adapter_cls(output_type)
            schema = self._output_type_adapter.json_schema()
            if not output_schema_name:
                output_schema_name = output_type.__name__
        self._core = ProviderCore.openai(
            api_key,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            top_p,
            max_tokens,
            schema,
            strict_json_schema,
            output_schema_name or "",
        )

    def chat(self, model: str, messages: list[dict[str, str]],
             tools: list["ToolSpec"] | None = None) -> ProviderResponse:
        """Send a chat completion request to the OpenAI API."""
        resp = self._core.chat(model, messages, tools)
        # A tool-calling turn has no JSON body to validate; the structured
        # answer comes on the turn after the results are fed back.
        if self._output_type_adapter is not None and not resp.tool_calls:
            try:
                resp.structured = self._output_type_adapter.validate_json(resp.content)
            except (self._validation_error, ValueError, TypeError) as exc:
                raw = resp.raw
                response_shape = {
                    "keys": list(raw.keys()) if isinstance(raw, dict) else [],
                    "choice_count": len(raw.get("choices", [])) if isinstance(raw, dict) else 0,
                }
                raise ProviderError(
                    f"Structured output parsing failed for {model}: {exc}. "
                    f"Response shape: {response_shape}"
                ) from exc
        return resp


class OpenAIOAuthProvider(OpenAIProvider):
    """OpenAI API provider using an OAuth token instead of an API key."""

    def __init__(
        self,
        oauth_token: str,
        base_url: str = OpenAIProvider.DEFAULT_BASE_URL,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        top_p: float = 1.0,
        max_tokens: int | None = None,
        output_type: type[Any] | None = None,
        strict_json_schema: bool = True,
        output_schema_name: str | None = None,
    ) -> None:
        super().__init__(
            api_key=oauth_token,
            base_url=base_url,
            timeout_sec=timeout_sec,
            retries=retries,
            backoff_sec=backoff_sec,
            interval_sec=interval_sec,
            temperature=temperature,
            top_p=top_p,
            max_tokens=max_tokens,
            output_type=output_type,
            strict_json_schema=strict_json_schema,
            output_schema_name=output_schema_name,
        )


class ClaudeProvider(Provider):
    """Anthropic Claude API provider."""

    DEFAULT_BASE_URL = "https://api.anthropic.com/v1"
    tool_dialect = ToolDialect.ANTHROPIC

    def __init__(
        self,
        api_key: str,
        base_url: str = DEFAULT_BASE_URL,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        max_tokens: int = 4096,
    ) -> None:
        self._core = ProviderCore.claude(
            api_key,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            max_tokens,
        )

    def chat(self, model: str, messages: list[dict[str, str]],
             tools: list["ToolSpec"] | None = None) -> ProviderResponse:
        """Send a messages request to the Claude API."""
        return self._core.chat(model, messages, tools)


class ClaudeOAuthProvider(ClaudeProvider):
    """Anthropic Claude API provider using an OAuth token.

    The only difference from :class:`ClaudeProvider` is the credential header,
    which is why the two share one backend.
    """

    def __init__(
        self,
        oauth_token: str,
        base_url: str = ClaudeProvider.DEFAULT_BASE_URL,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        max_tokens: int = 4096,
    ) -> None:
        self._core = ProviderCore.claude(
            oauth_token,
            base_url,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            max_tokens,
            True,
        )


class CustomProvider(Provider):
    """Generic provider for any OpenAI-compatible API endpoint.

    Works with vendors like Alibaba Cloud (DashScope/Bailian), z.ai,
    Together AI, Fireworks, and any service exposing an OpenAI-compatible
    chat completions endpoint.

    Args:
        url: Base URL of the API (e.g. ``"https://coding.dashscope.aliyuncs.com/v1"``).
        api_key: API key for authentication (sent as ``Bearer`` token).
        model_config: Optional dict describing the model — mirrors vendor
            config JSON (``maxTokens``, ``contextWindow``, ``reasoning``,
            ``compat``, etc.). ``maxTokens`` is used as the default for
            *max_tokens* when the explicit parameter is not set. The full
            dict is stored and accessible via the :attr:`model_config`
            property.
        timeout_sec: HTTP request timeout in seconds.
        retries: Number of retry attempts on failure.
        backoff_sec: Base backoff delay between retries.
        interval_sec: Fixed interval between retries (overrides backoff).
        temperature: Sampling temperature.
        top_p: Nucleus sampling parameter.
        max_tokens: Maximum tokens to generate (overrides ``model_config["maxTokens"]``).
        extra_headers: Additional HTTP headers merged into every request.
        extra_body: Additional fields merged into the JSON request body.
    """

    #: Assumed OpenAI-shaped, since that is what "OpenAI-compatible" means.
    #: Endpoints that advertise compatibility without implementing function
    #: calling degrade to TEXT on their first 400 — see
    #: :meth:`Provider.note_native_tools_rejected`.
    tool_dialect = ToolDialect.OPENAI

    def __init__(
        self,
        url: str,
        api_key: str,
        model_config: dict[str, Any] | None = None,
        timeout_sec: int = 60,
        retries: int = 2,
        backoff_sec: float = 2.0,
        interval_sec: float | None = None,
        temperature: float = 1.0,
        top_p: float = 1.0,
        max_tokens: int | None = None,
        extra_headers: dict[str, str] | None = None,
        extra_body: dict[str, Any] | None = None,
    ) -> None:
        self._model_config: dict[str, Any] = dict(model_config) if model_config else {}
        self._core = ProviderCore.custom(
            url,
            api_key,
            self._model_config,
            timeout_sec,
            retries,
            backoff_sec,
            interval_sec,
            temperature,
            top_p,
            max_tokens,
            extra_headers,
            extra_body,
        )

    @property
    def model_config(self) -> dict[str, Any]:
        """Return a copy of the model configuration dict."""
        return dict(self._model_config)

    def chat(self, model: str, messages: list[dict[str, str]],
             tools: list["ToolSpec"] | None = None) -> ProviderResponse:
        """Send a chat completion request to the custom endpoint."""
        return self._core.chat(model, messages, tools)
