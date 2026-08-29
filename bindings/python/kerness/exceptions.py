"""Kerness exceptions.

Declared in Python rather than in the extension module because two of them
take more than a message: `ProviderHTTPError` and `ProviderNetworkError` keep
their pieces as attributes, and code that catches one reads them back.
"""

from __future__ import annotations


class KernessError(Exception):
    """Base exception for all Kerness errors."""


class ProviderError(KernessError):
    """Base exception for provider errors."""


class ProviderHTTPError(ProviderError):
    """HTTP error from the LLM provider."""

    def __init__(self, status_code: int, url: str, body: str = "") -> None:
        self.status_code = status_code
        self.url = url
        self.body = body
        super().__init__(f"HTTP {status_code} from {url}: {body}")


class ProviderNetworkError(ProviderError):
    """Network-level error reaching the provider."""

    def __init__(self, url: str, cause: Exception | None = None) -> None:
        self.url = url
        self.cause = cause
        super().__init__(f"Network error for {url}: {cause}")


class ProviderEmptyResponseError(ProviderError):
    """Provider returned an empty response."""


class SessionError(KernessError):
    """Error during session execution."""


class GameplanLoadError(KernessError):
    """Error loading a gameplan file."""


class AccessDeniedError(KernessError):
    """Access denied by access control policy."""


__all__ = [
    "KernessError",
    "ProviderError",
    "ProviderHTTPError",
    "ProviderNetworkError",
    "ProviderEmptyResponseError",
    "SessionError",
    "GameplanLoadError",
    "AccessDeniedError",
]
