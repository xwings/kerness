"""Console, file, JSONL, and fan-out output.

The four bundled channels are the Rust crate's, re-exported here. What they
write, how they stamp it, and the fan-out's rule that one member's failure does
not stop the others are all decided there; the console's line reaches
``sys.stdout`` and a failed delivery reaches ``logging`` because the crate asks
the binding to deliver them, not because they are implemented in Python.

:class:`Channel` stays Python because it is what callers subclass, and an
abstract base class is something the extension cannot declare. The four
concrete channels are registered as virtual subclasses of it, so
``isinstance(ConsoleChannel(), Channel)`` holds.
"""

from abc import ABC, abstractmethod
from pathlib import Path

from kerness._core import ConsoleChannel, FileChannel, LogChannel, MultiChannel


class Channel(ABC):
    """Abstract base class for output channels."""

    @abstractmethod
    def send(self, sender: str, message: str) -> None:
        """Send a message attributed to a sender."""

    @abstractmethod
    def send_system(self, message: str) -> None:
        """Send a system-level message."""

    def paths(self) -> list[Path]:
        """Files this channel writes, for the session workspace to confine.

        Concrete rather than abstract because most channels write no file at
        all — a console prints, and a caller's remote channel posts.
        Overriding it is how a file-backed channel opts into the containment
        check the memory file and the session file already go through.

        Read once, when the session binds the channel, so a channel that picks
        its destination later is not confined by the workspace.
        """
        return []


# Registered rather than inherited: an extension type cannot subclass a Python
# ABC, and what the base class is actually for is the isinstance check and the
# subclassing seam, both of which registration preserves.
for _channel in (ConsoleChannel, FileChannel, LogChannel, MultiChannel):
    Channel.register(_channel)
del _channel


__all__ = [
    "Channel",
    "ConsoleChannel",
    "FileChannel",
    "LogChannel",
    "MultiChannel",
]
