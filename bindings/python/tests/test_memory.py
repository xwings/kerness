"""Tests for kerness.memory."""

from pathlib import Path

import pytest

from kerness.access import AccessPolicy
from kerness.memory import FileMemory, Memory, MemoryStore
from kerness.session import Session


class TestMemory:
    def test_load_reads_what_is_there_and_creates_what_is_not(self, tmp_path):
        """A read-only session must leave no trace on disk.

        Creating the file and stamping a '# Memory' heading into it would
        both touch a file the caller may never have wanted and impose a format
        on a file that is theirs to shape.
        """
        missing = tmp_path / "absent.md"
        mem = Memory(str(missing))
        mem.load()
        assert not missing.exists()
        assert mem.read() == ""
        assert mem.path == missing

        existing = tmp_path / "memory.md"
        existing.write_text("# My Memory\n\nSome notes.\n", encoding="utf-8")
        loaded = Memory(str(existing))
        loaded.load()
        assert loaded.read() == "# My Memory\n\nSome notes.\n"

    def test_append_and_write_update_the_cache_and_the_file_together(self, tmp_path):
        """The cache is what reaches the prompt and the file is what outlives
        the run; the two drifting apart shows up only on the next session."""
        path = tmp_path / "memory.md"
        path.write_text("old content", encoding="utf-8")
        mem = Memory(str(path))
        mem.load()

        mem.append("\n## Notes\n- first note\n")
        assert "first note" in mem.read()
        assert "first note" in path.read_text(encoding="utf-8")

        mem.write("new content")
        assert mem.read() == "new content"
        assert path.read_text(encoding="utf-8") == "new content"

    def test_an_entry_is_stored_verbatim_one_blank_line_apart(self, tmp_path):
        """The file is the user's prose; nothing is wrapped around an entry.

        Wrapping entries in a '### {agent} (turn {n})' heading and '- '
        bullets would impose a shape on a file the framework does not own.
        """
        path = tmp_path / "memory.md"
        path.write_text("alice goes to school by bus\n", encoding="utf-8")
        mem = Memory(str(path))
        mem.load()

        mem.append_entry("alice arrive school at 7.30am")
        mem.append_entry("she leaves at 3pm")

        assert path.read_text(encoding="utf-8") == (
            "alice goes to school by bus\n\n"
            "alice arrive school at 7.30am\n\n"
            "she leaves at 3pm\n"
        )

    def test_nothing_reaches_disk_until_there_is_something_to_write(self, tmp_path):
        """load() does not touch disk, so the first real write does the
        mkdir — and a blank note is not a real write."""
        path = tmp_path / "sub" / "dir" / "memory.md"
        mem = Memory(str(path))
        mem.load()

        mem.append_entry("   \n  ")
        assert mem.read() == ""
        assert not path.exists()

        mem.append_entry("a note")
        assert path.read_text(encoding="utf-8") == "a note\n"

    def test_age_is_none_without_a_file_and_whole_days_once_there_is_one(
        self, tmp_path
    ):
        """`Option<u64>` reaching Python as ``None`` or an ``int``: the prompt's
        staleness caveat is the file's own age, so a file that does not exist
        has to be distinguishable from one written today."""
        import os
        import time

        path = tmp_path / "memory.md"
        mem = Memory(str(path))
        mem.load()
        assert mem.age is None

        mem.append_entry("a note")
        assert mem.age == 0

        week = time.time() - 7 * 24 * 60 * 60
        os.utime(path, (week, week))
        mem.load()
        assert mem.age == 7


class TestMemoryStore:
    """The slot: a store the caller wrote, seen by the framework."""

    def test_the_base_class_answers_for_a_store_that_keeps_no_file(self):
        """Only ``read`` and ``append`` are required. The rest have answers a
        store keeping nothing on disk can live with, so the smallest possible
        store is two methods."""

        class Ephemeral(MemoryStore):
            def __init__(self):
                self.notes = []

            def read(self, scope):
                return "\n".join(self.notes)

            def append(self, scope, note):
                self.notes.append(note)

        store = Ephemeral()
        assert store.open("anything") is None
        assert store.age("anything") is None
        assert store.path("anything") is None
        assert store.close() is None

    def test_the_bundled_store_is_a_memory_store(self):
        """Registered rather than inherited, so ``isinstance`` holds against
        an extension type the ABC cannot be a base of."""
        assert isinstance(FileMemory(), MemoryStore)
        assert issubclass(FileMemory, MemoryStore)

    def test_the_bundled_store_keeps_one_file_per_scope(self, tmp_path):
        """A scope is a name the store interprets, and this one reads it as a
        path: two scopes are two files and neither sees the other's notes."""
        store = FileMemory()
        alice = str(tmp_path / "alice.md")
        bob = str(tmp_path / "bob.md")

        store.open(alice)
        store.append(alice, "Alice was here.")
        store.append(bob, "Bob was here.")

        assert store.read(alice) == "Alice was here.\n"
        assert store.read(bob) == "Bob was here.\n"
        assert store.path(alice) == Path(alice)
        assert store.age(alice) == 0
        assert store.close() is None

    def test_a_store_that_raises_reaches_the_caller_as_what_it_raised(
        self, tmp_path
    ):
        """``read`` and ``append`` are fallible on both sides, so the class the
        store raised is the class the caller catches — not a framework error
        the exception was flattened into."""

        class Sealed(MemoryStore):
            def read(self, scope):
                raise ValueError("sealed")

            def append(self, scope, note):
                raise ValueError("sealed")

        session = Session(
            gameplan="debate",
            topic="T",
            memory="anywhere",
            memory_store=Sealed(),
            access_policy=AccessPolicy(workspace=tmp_path),
        )
        with pytest.raises(ValueError, match="sealed"):
            session.memory.read()
