"""Shared test fixtures."""

import pytest

from kerness.channel import Channel
from kerness.provider import Provider, ProviderResponse


class MockProvider(Provider):
    """Provider that returns canned responses and tracks calls."""

    def __init__(self, responses: list[str] | None = None,
                 response_map: dict[str, str] | None = None) -> None:
        super().__init__(retries=0, backoff_sec=0)
        self._responses = list(responses) if responses else []
        self._response_map = response_map or {}
        self._call_index = 0
        self.calls: list[dict] = []

    def chat(self, model: str, messages: list[dict[str, str]]) -> ProviderResponse:
        self.calls.append({"model": model, "messages": messages})
        content = self._get_response()
        return ProviderResponse(content=content, model=model)

    def _get_response(self) -> str:
        if self._responses:
            idx = min(self._call_index, len(self._responses) - 1)
            self._call_index += 1
            return self._responses[idx]
        self._call_index += 1
        return f"Mock response {self._call_index}"


class PurposeMockProvider(Provider):
    """Provider that returns different responses based on the purpose string."""

    def __init__(self, purpose_map: dict[str, str],
                 default: str = "Mock response") -> None:
        super().__init__(retries=0, backoff_sec=0)
        self._purpose_map = purpose_map
        self._default = default
        self.calls: list[dict] = []

    def chat(self, model: str, messages: list[dict[str, str]]) -> ProviderResponse:
        self.calls.append({"model": model, "messages": messages})
        return ProviderResponse(content=self._default, model=model)

    def chat_with_retries(self, model: str, messages: list[dict[str, str]],
                          purpose: str = "") -> ProviderResponse:
        self.calls.append({"model": model, "messages": messages, "purpose": purpose})
        for key, response in self._purpose_map.items():
            if key in purpose:
                return ProviderResponse(content=response, model=model)
        return ProviderResponse(content=self._default, model=model)


class SequenceMockProvider(Provider):
    """Provider that returns responses in order, cycling through the list."""

    def __init__(self, responses: list[str]) -> None:
        super().__init__(retries=0, backoff_sec=0)
        self._responses = responses
        self._call_index = 0
        self.calls: list[dict] = []

    def chat(self, model: str, messages: list[dict[str, str]]) -> ProviderResponse:
        self.calls.append({"model": model, "messages": messages})
        return self._next_response(model)

    def chat_with_retries(self, model: str, messages: list[dict[str, str]],
                          purpose: str = "") -> ProviderResponse:
        self.calls.append({"model": model, "messages": messages, "purpose": purpose})
        return self._next_response(model)

    def _next_response(self, model: str) -> ProviderResponse:
        if not self._responses:
            return ProviderResponse(content="(no responses configured)", model=model)
        idx = min(self._call_index, len(self._responses) - 1)
        self._call_index += 1
        return ProviderResponse(content=self._responses[idx], model=model)


class CaptureChannel(Channel):
    """Channel that records all messages to a list."""

    def __init__(self) -> None:
        self.messages: list[dict[str, str]] = []

    def send(self, sender: str, message: str) -> None:
        self.messages.append({"type": "send", "sender": sender, "message": message})

    def send_system(self, message: str) -> None:
        self.messages.append({"type": "system", "sender": "system", "message": message})


@pytest.fixture
def mock_provider():
    return MockProvider(responses=["Mock response"])


@pytest.fixture
def capture_channel():
    return CaptureChannel()
