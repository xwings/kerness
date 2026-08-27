"""Per-API tool schema conversion, call parsing, and message rendering.

The two native APIs disagree about every part of tool calling: where the
schema goes, what the schema key is called, how a call comes back, and how a
result is fed in. Each dialect gets its own converter and everything above
stays dialect-neutral. ``TEXT`` is the fallback for endpoints with no native
support — the only dialect every provider can speak.
"""

from kerness._core import (
    parse_anthropic_tool_calls,
    parse_openai_tool_calls,
    render_assistant_turn,
    render_tool_result,
    to_anthropic_tool,
    to_openai_tool,
    tool_schemas,
)
from kerness._enums import ToolDialect

__all__ = [
    "ToolDialect",
    "parse_anthropic_tool_calls",
    "parse_openai_tool_calls",
    "render_assistant_turn",
    "render_tool_result",
    "to_anthropic_tool",
    "to_openai_tool",
    "tool_schemas",
]
