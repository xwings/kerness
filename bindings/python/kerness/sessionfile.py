"""One run's machine state on disk, plus the identity check that guards it."""

from kerness._core import (
    SCHEMA_VERSION,
    SessionSnapshot,
    check_identity,
    identity_for,
    load_snapshot,
    save_snapshot,
)

__all__ = [
    "SCHEMA_VERSION",
    "SessionSnapshot",
    "check_identity",
    "identity_for",
    "load_snapshot",
    "save_snapshot",
]
