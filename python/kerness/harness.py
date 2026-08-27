"""The harness contract: schema, parsing, and validation."""

from kerness._core import (
    RESERVED_TOOL_NAMES,
    AgentsSpec,
    HarnessSpec,
    LoopSpec,
    OrchestratorSpec,
    ParticipantSpec,
    PhaseSpec,
    ResultField,
    parse_harness,
    validate_harness,
)

__all__ = [
    "RESERVED_TOOL_NAMES",
    "AgentsSpec",
    "HarnessSpec",
    "LoopSpec",
    "OrchestratorSpec",
    "ParticipantSpec",
    "PhaseSpec",
    "ResultField",
    "parse_harness",
    "validate_harness",
]
