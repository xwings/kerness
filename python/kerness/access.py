"""Default-deny command and path policy.

The policy is a Python dataclass rather than an extension class, because its
contract is written in Python list semantics: a caller may build one, hand it
to a manager, and then append to one of its lists — and the manager is
required *not* to see that, because it snapshots at construction. The manager
itself, and every decision it makes, is Rust.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from kerness._core import AccessManager, AccessRequest

ApprovePrompt = Callable[[AccessRequest], bool]


def prompt_on_console(req: AccessRequest) -> bool:
    """Ask a human on the console whether to approve *req*.

    **Opt-in only.** A session is a one-off non-interactive cycle, so nothing
    reaches for this unless a caller passes it explicitly::

        AccessPolicy(approve_prompt=prompt_on_console)

    Off a TTY there is no human to answer, so this denies rather than reading a
    piped stdin or blocking on one that never closes. The check lives here, in
    the one approver that actually needs a console — a caller whose approver is
    a GUI dialog or an HTTP callback has nothing to do with stdin and is never
    gated on it.

    Args:
        req: The access request to approve.

    Returns:
        True when the human approved. An empty answer means yes; EOF, and a
        non-interactive stdin, mean no.
    """
    if not _stdin_is_interactive():
        return False
    actor = f"Agent: {req.actor}\n" if req.actor else ""
    message = (
        "Approve request\n"
        f"{actor}"
        f"Type: {req.kind} {req.action}\n"
        f"Target: {req.target}\n"
    )
    message = _blue(message)
    try:
        answer = input(f"{message}{_blue('Approve? [Y/n]: ')}")
    except EOFError:
        return False
    if not answer.strip():
        return True
    return answer.strip().lower() in {"y", "yes"}


def _stdin_is_interactive() -> bool:
    """Whether a console prompt has any chance of being answered."""
    try:
        import sys

        return bool(sys.stdin) and sys.stdin.isatty()
    except Exception:
        return False


def _blue(text: str) -> str:
    try:
        import sys

        if sys.stdout.isatty():
            return f"\033[34m{text}\033[0m"
    except Exception:
        pass
    return text


@dataclass
class AccessPolicy:
    """Policy describing allowed and blocked access patterns.

    **The default refuses rather than asks.** A session is a one-off cycle
    that runs to completion with no human in the loop, so an unlisted request
    raises :class:`~kerness.exceptions.AccessDeniedError` — which
    :meth:`~kerness.toolkit.ToolDispatcher.execute` turns into an error tool
    result the agent reads and works around. Denial costs the calling agent a
    tool result; blocking on ``input()`` would cost the whole session.

    Pass ``approve_prompt=prompt_on_console`` to opt back into asking.
    """

    approve_prompt: ApprovePrompt | None = None
    auto_approve_prefixes: list[str] = field(default_factory=list)

    allowed_programs: list[str] = field(default_factory=list)
    allowed_commands: list[str] = field(default_factory=list)
    allowed_prefixes: list[str] = field(default_factory=list)
    allowed_command_patterns: list[str] = field(default_factory=list)

    allowed_files: list[str | Path] = field(default_factory=list)
    allowed_dirs: list[str | Path] = field(default_factory=list)

    #: Whether activating a skill grants read access to the ``scripts/`` and
    #: ``references/`` directories it bundles. Activating a skill is a real
    #: privilege grant, so it is only ever extended to skills that ship inside
    #: the package; a skill loaded from a user-supplied path never widens the
    #: policy, regardless of this flag.
    trust_skill_bundles: bool = True


__all__ = [
    "AccessManager",
    "AccessPolicy",
    "AccessRequest",
    "ApprovePrompt",
    "prompt_on_console",
]
