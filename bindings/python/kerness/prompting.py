"""Participant and orchestrator prompt assembly."""

from kerness._core import (
    CONTEXT_HEADER,
    MEMORY_HEADER,
    MEMORY_STALE_AFTER_DAYS,
    MEMORY_WRITE_HINT,
    PromptAssembler,
    context_block,
    memory_block,
    memory_freshness,
)

__all__ = [
    "CONTEXT_HEADER",
    "MEMORY_HEADER",
    "MEMORY_STALE_AFTER_DAYS",
    "MEMORY_WRITE_HINT",
    "PromptAssembler",
    "context_block",
    "memory_block",
    "memory_freshness",
]
