"""The tool dialect, as a real Python enum.

Callers compare dialects with ``is``, so the three members have to be enum
members rather than values reconstructed at each boundary crossing. Declaring
the class here and handing it down to the extension module is what makes
``provider.effective_dialect() is ToolDialect.TEXT`` answer the way it reads.
"""

from __future__ import annotations

from enum import Enum


class ToolDialect(str, Enum):
    """How a provider expects tool schemas and tool results to be encoded.

    ``TEXT`` is the fallback: tools are described in prose and calls are
    scraped out of a fenced JSON block. ``OPENAI`` and ``ANTHROPIC`` send real
    schemas in the request and read structured tool-use blocks back.
    """

    TEXT = "text"
    OPENAI = "openai"
    ANTHROPIC = "anthropic"
