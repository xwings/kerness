"""Role parsing, resolution, and discovery."""

from kerness._core import (
    DEFAULT_ROLE_FILE,
    RoleConfig,
    list_builtin_roles,
    load_role,
    resolve_role_path,
)

__all__ = [
    "DEFAULT_ROLE_FILE",
    "RoleConfig",
    "list_builtin_roles",
    "load_role",
    "resolve_role_path",
]
