"""Where a session keeps what its agents remember.

The default store is the Rust crate's, re-exported here. Which scope an agent
addresses, how a note is separated from the one before it, and what an absent
file reads as are all decided there.

:class:`MemoryStore` stays Python because it is what callers subclass, and an
abstract base class is something the extension cannot declare. Every bundled
store is registered as a virtual subclass of it, so
``isinstance(FileMemory(), MemoryStore)`` holds.
"""

from abc import ABC, abstractmethod
from pathlib import Path

from kerness._core import (
    CONSOLIDATE_PROMPT,
    CONSOLIDATED_PREFIX,
    DEFAULT_KEEP_ENTRIES,
    DEFAULT_MEMORY_BUDGET,
    ENTRY_SEPARATOR,
    REVISE_UNSUPPORTED,
    CuratedMemory,
    FileMemory,
    Memory,
    SessionMemory,
    SummarizingMemory,
)


class MemoryStore(ABC):
    """Abstract base class for memory stores.

    A scope is a name the store interprets. The session hands it the string it
    was configured with — :class:`FileMemory` reads that as a path, another
    store as a key — so nothing above this class assumes memory is a file.
    """

    @abstractmethod
    def read(self, scope: str) -> str:
        """Everything stored under *scope*, as the prompt should quote it."""

    @abstractmethod
    def append(self, scope: str, note: str) -> None:
        """Store *note* under *scope*, as its own entry.

        The note arrives exactly as the writer wrote it, and has already
        passed whatever ``memory_filter`` the session installed: a store is
        never the place that filter is enforced, so installing one cannot skip
        it.
        """

    def open(self, scope: str) -> None:
        """Make *scope* ready to read and write.

        Called once per scope before the first turn, so a store that cannot
        reach its backing fails the run before a provider has been paid for a
        turn against it. Concrete because a store with nothing to prepare has
        nothing to say here.
        """

    def age(self, scope: str) -> int | None:
        """Whole days since *scope* was last written, or ``None``.

        The prompt marks memory as stale past a day. A store that cannot date
        a scope answers ``None`` and the mark is left off.
        """
        return None

    def path(self, scope: str) -> Path | None:
        """The file *scope* is kept in, for the session workspace to confine.

        Concrete rather than abstract because a store need not keep files at
        all. Overriding it is how a file-backed store opts into the
        containment check the session file already goes through.
        """
        return None

    def close(self) -> None:
        """The store's last word, after everything that could write has.

        Called once at the end of ``run()``. A store that consolidates,
        indexes, or flushes does it here.
        """

    def budget(self) -> int | None:
        """The character ceiling one scope may hold, or ``None`` for none.

        Answering makes the store *curated*: the session registers the
        ``edit_memory`` tool exactly when this is not ``None``, and states the
        figure in the tool's description. A store that answers must implement
        :meth:`revise`, because that is the only way an agent gets back under
        the ceiling it is being held to.
        """
        return None

    def revise(self, scope: str, old: str, new: str) -> None:
        """Replace the one entry containing *old*, or remove it when *new* is
        empty.

        *old* addresses an entry by being a substring of exactly one of them;
        matching none or several should raise rather than rewrite a guess.
        *new* replaces the whole entry and has passed the session's
        ``memory_filter`` exactly as an appended note does.

        Concrete rather than abstract, and a refusal rather than a no-op: a
        store that keeps notes append-only has no entry to address, and
        reporting a revision it did not make is the one failure its caller
        cannot detect. The message is the crate's, imported rather than
        respelled, so the two halves of the trait's default cannot drift.
        """
        raise ValueError(REVISE_UNSUPPORTED)


# Registered rather than inherited: an extension type cannot subclass a Python
# ABC, and what the base class is actually for is the isinstance check and the
# subclassing seam, both of which registration preserves.
MemoryStore.register(FileMemory)
MemoryStore.register(SummarizingMemory)
MemoryStore.register(CuratedMemory)


__all__ = [
    "CONSOLIDATE_PROMPT",
    "CONSOLIDATED_PREFIX",
    "DEFAULT_KEEP_ENTRIES",
    "DEFAULT_MEMORY_BUDGET",
    "ENTRY_SEPARATOR",
    "REVISE_UNSUPPORTED",
    "CuratedMemory",
    "FileMemory",
    "Memory",
    "MemoryStore",
    "SessionMemory",
    "SummarizingMemory",
]
