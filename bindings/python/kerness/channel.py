"""Console, file, JSONL, and fan-out output.

The four bundled channels are Python, unlike the rest of the framework, and
for the same reason in each case: what they do *is* Python I/O. A console
channel is a ``print``, so it has to reach ``sys.stdout`` and not file
descriptor 1; a fan-out reports a failed delivery through ``logging``, so it
has to reach the caller's handlers.
"""

import json
import logging
from abc import ABC, abstractmethod
from datetime import datetime, timezone
from pathlib import Path


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


class ConsoleChannel(Channel):
    """Prints messages to stdout."""

    def __init__(self, prefix_format: str = "[{sender}]") -> None:
        self._prefix_format = prefix_format

    def send(self, sender: str, message: str) -> None:
        prefix = self._prefix_format.format(sender=sender)
        print(f"{prefix} {message}", flush=True)

    def send_system(self, message: str) -> None:
        print(f"[System] {message}", flush=True)


class MultiChannel(Channel):
    """Fan-out to multiple channels.

    One channel's failure does not stop the others.  Channels are typically
    mixed local and remote — a console plus a log file plus whatever chat
    transport the caller wrote — and a network blip on the remote one must not
    cost the session its local transcript, nor abort the run partway through a
    turn.  A failure is logged and the fan-out continues.
    """

    def __init__(self, *channels: Channel) -> None:
        self._channels = list(channels)

    def send(self, sender: str, message: str) -> None:
        self._fan_out(lambda ch: ch.send(sender, message))

    def send_system(self, message: str) -> None:
        self._fan_out(lambda ch: ch.send_system(message))

    def paths(self) -> list[Path]:
        """Every path its members write, so wrapping a channel does not hide it
        from the session workspace."""
        return [path for ch in self._channels for path in ch.paths()]

    def _fan_out(self, deliver) -> None:
        for ch in self._channels:
            try:
                deliver(ch)
            except Exception:  # noqa: BLE001 — one channel must not sink the rest
                logging.exception(
                    "Channel %s failed to deliver; continuing",
                    type(ch).__name__,
                )


class LogChannel(Channel):
    """Writes messages as JSONL to a log file."""

    def __init__(self, log_dir: str = "logs") -> None:
        log_path = Path(log_dir)
        log_path.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self._log_path = log_path / f"session_{stamp}.jsonl"

    def send(self, sender: str, message: str) -> None:
        self._write_event({"role": "assistant", "sender": sender, "content": message})

    def send_system(self, message: str) -> None:
        self._write_event({"role": "system", "sender": "system", "content": message})

    def paths(self) -> list[Path]:
        return [self._log_path]

    def _write_event(self, payload: dict) -> None:
        payload["ts"] = datetime.now(timezone.utc).isoformat()
        with self._log_path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(payload, ensure_ascii=True) + "\n")


class FileChannel(Channel):
    """Writes messages as plain text to a file."""

    def __init__(self, filepath: str | Path) -> None:
        self._filepath = Path(filepath)

    def send(self, sender: str, message: str) -> None:
        with self._filepath.open("a", encoding="utf-8") as f:
            f.write(f"[{sender}] {message}\n")

    def send_system(self, message: str) -> None:
        with self._filepath.open("a", encoding="utf-8") as f:
            f.write(f"[System] {message}\n")

    def paths(self) -> list[Path]:
        return [self._filepath]


__all__ = [
    "Channel",
    "ConsoleChannel",
    "FileChannel",
    "LogChannel",
    "MultiChannel",
]
