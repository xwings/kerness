"""Default-deny command and path policy.

The policy is a Python dataclass rather than an extension class, because its
contract is written in Python list semantics: a caller may build one, hand it
to a manager, and then append to one of its lists — and the manager is
required *not* to see that, because it snapshots at construction. The manager
itself, every decision it makes, and the console approver below are Rust.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

from kerness._core import AccessManager, AccessRequest, prompt_on_console

ApprovePrompt = Callable[[AccessRequest], bool]


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

    #: The directory the session works in, or ``None`` for the process's own
    #: current directory. It *grants*: every path under it is reachable without
    #: an allowlist entry, so a session pointed at ``/opt/harness`` reads that
    #: tree and its subdirectories on the strength of the workspace alone. It
    #: also becomes the working directory a command starts in. Reaching further
    #: is :attr:`allowed_dirs`' job, not an approver's.
    workspace: str | Path | None = None

    #: Workspaces for named agents, each of which *narrows* :attr:`workspace`.
    #: The one option that does not simply override the session's: a replaceable
    #: workspace would let an agent hand itself more of the filesystem than the
    #: session was given. A workspace outside the session's is refused at
    #: :meth:`~kerness.session.Session.run`, naming the agent. Usually written
    #: by passing ``workspace=`` to an agent rather than filled in here.
    agent_workspaces: dict[str, str | Path] = field(default_factory=dict)

    #: Commands agents may run, as anchored globs over the whole command line.
    #: ``*`` stands for any run of characters, including none: ``["*"]`` allows
    #: every command, ``"git *"`` any git invocation carrying arguments, and a
    #: pattern with no ``*`` is an exact match. Anchored rather than searched —
    #: unlike :attr:`allowed_command_patterns` — so ``"git *"`` cannot admit
    #: ``sudo git push``. Empty is the default, and it allows nothing.
    allowed_commands: list[str] = field(default_factory=list)
    #: Commands agents may run, as regexes searched anywhere in the line. The
    #: unanchored counterpart to :attr:`allowed_commands`, and the looser of the
    #: two.
    allowed_command_patterns: list[str] = field(default_factory=list)

    #: Files and directories reachable *in addition to* :attr:`workspace`. This
    #: is how a session confined to one project still reads ``/tmp``. The
    #: workspace and these together are the whole of what a session can touch.
    allowed_files: list[str | Path] = field(default_factory=list)
    #: Directories reachable in addition to the workspace — see
    #: :attr:`allowed_files`.
    allowed_dirs: list[str | Path] = field(default_factory=list)

    #: Hosts a command may name, as anchored globs over the hostname —
    #: ``"example.com"`` exactly, ``"*.example.com"`` for its subdomains but not
    #: itself, ``"*"`` for any. This *narrows*, and it is the one allow-list here
    #: that is empty-means-open rather than empty-means-nothing: a command must
    #: already be permitted by :attr:`allowed_commands` or an approver before
    #: this is consulted, so an empty list changes nothing and a non-empty one
    #: takes URLs back off a command that was otherwise allowed. What is checked
    #: is the URLs written on the command line, which is what confines a session
    #: running ``agent-browser open <url>`` to the sites it has business with.
    allowed_hosts: list[str] = field(default_factory=list)

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
