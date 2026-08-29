"""Kerness: framework for building multi-agent harnesses."""

from __future__ import annotations

from pathlib import Path as _Path

from kerness import _core, _enums, exceptions

# The extension module needs three things it cannot declare itself: the
# exception classes, the dialect enum, and where the bundled gameplans, roles,
# personas, and skills were installed. This is the only place that knows.
_core.bootstrap(exceptions, _enums.ToolDialect, str(_Path(__file__).resolve().parent))

__version__ = _core.__version__

from kerness.access import (
    AccessManager,
    AccessPolicy,
    AccessRequest,
    prompt_on_console,
)
from kerness.agent import Agent
from kerness.channel import (
    Channel,
    ConsoleChannel,
    FileChannel,
    LogChannel,
    MultiChannel,
)
from kerness.exceptions import (
    AccessDeniedError,
    GameplanLoadError,
    KernessError,
    ProviderEmptyResponseError,
    ProviderError,
    ProviderHTTPError,
    ProviderNetworkError,
    SessionError,
)
from kerness.gameplan_loader import GameplanConfig, load_gameplan
from kerness.memory import Memory
from kerness.persona_loader import (
    PersonaConfig,
    format_persona_for_prompt,
    list_builtin_personas,
    load_persona,
)
from kerness.provider import (
    ClaudeOAuthProvider,
    ClaudeProvider,
    CustomProvider,
    OpenAIOAuthProvider,
    OpenAIProvider,
    OpenRouterProvider,
    Provider,
    ProviderResponse,
)
from kerness.role_loader import (
    RoleConfig,
    list_builtin_roles,
    load_role,
)
from kerness.session import Message, Session, SessionResult
from kerness.skill_loader import (
    SkillConfig,
    list_builtin_skills,
    load_skill,
)
from kerness.skill_runtime import SKILL_TOOL_NAME, SkillRegistry, format_skills_index
from kerness.tooling import ToolCall, ToolSpec
from kerness.toolschema import ToolDialect

__all__ = [
    # Core
    "Session",
    "SessionResult",
    "Message",
    # Agent
    "Agent",
    # Provider
    "Provider",
    "OpenRouterProvider",
    "OpenAIProvider",
    "OpenAIOAuthProvider",
    "ClaudeProvider",
    "ClaudeOAuthProvider",
    "CustomProvider",
    "ProviderResponse",
    # Access
    "AccessPolicy",
    "AccessManager",
    "AccessRequest",
    "prompt_on_console",
    # Channel
    "Channel",
    "ConsoleChannel",
    "FileChannel",
    "LogChannel",
    "MultiChannel",
    # Gameplan
    "GameplanConfig",
    "load_gameplan",
    # Role
    "RoleConfig",
    "load_role",
    "list_builtin_roles",
    # Persona
    "PersonaConfig",
    "load_persona",
    "list_builtin_personas",
    "format_persona_for_prompt",
    # Skills
    "SkillConfig",
    "load_skill",
    "list_builtin_skills",
    "format_skills_index",
    "SkillRegistry",
    "SKILL_TOOL_NAME",
    # Tools
    "ToolSpec",
    "ToolCall",
    "ToolDialect",
    # Memory
    "Memory",
    # Exceptions
    "KernessError",
    "AccessDeniedError",
    "ProviderError",
    "ProviderHTTPError",
    "ProviderNetworkError",
    "ProviderEmptyResponseError",
    "SessionError",
    "GameplanLoadError",
]
