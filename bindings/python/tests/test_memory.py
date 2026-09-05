"""Tests for kerness.memory."""

from pathlib import Path

import pytest

from kerness.access import AccessPolicy
from kerness.memory import (
    CONSOLIDATED_PREFIX,
    DEFAULT_MEMORY_BUDGET,
    REVISE_UNSUPPORTED,
    CuratedMemory,
    FileMemory,
    Memory,
    MemoryStore,
    SummarizingMemory,
)
from kerness.session import Session
from tests.conftest import MockProvider


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
        assert store.maintenance_scopes() == []
        assert store.maintain_scope("anything") is None
        assert store.close_run() is None
        assert store.budget() is None
        with pytest.raises(ValueError, match="cannot be revised"):
            store.revise("anything", "old", "new")
        # The refusal is the crate's own message, imported rather than
        # respelled here, so the two halves of the default cannot drift.
        assert REVISE_UNSUPPORTED.endswith("cannot be revised or removed")

    def test_the_bundled_stores_are_memory_stores(self, tmp_path):
        """Registered rather than inherited, so ``isinstance`` holds against
        an extension type the ABC cannot be a base of."""
        summarizing = SummarizingMemory(tmp_path, MockProvider(), "m")
        assert isinstance(FileMemory(), MemoryStore)
        assert issubclass(FileMemory, MemoryStore)
        assert isinstance(summarizing, MemoryStore)
        assert issubclass(SummarizingMemory, MemoryStore)
        assert isinstance(CuratedMemory(tmp_path), MemoryStore)
        assert issubclass(CuratedMemory, MemoryStore)
        for store in [FileMemory(), summarizing, CuratedMemory(tmp_path)]:
            assert store.maintenance_scopes() == []
            assert store.close_run() is None

        for base, arguments in [(FileMemory, ()), (CuratedMemory, (tmp_path,))]:
            class WithCleanup(base):
                closed = 0

                def close(self):
                    self.closed += 1
                    super().close()

            store = WithCleanup(*arguments)
            assert store.close_run() is None
            assert store.closed == 1, f"{base.__name__} skipped subclass cleanup"

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


class TestSummarizingMemory:
    """The second bundled store: recent notes verbatim, the rest summarised."""

    def test_entries_stay_verbatim_until_the_run_closes(self, tmp_path):
        """Nothing is rewritten while the session is still writing: the store
        summarises once, at the end, when the whole run is known."""
        provider = MockProvider(responses=["a compact recap"])
        store = SummarizingMemory(tmp_path, provider, "test-model", keep=2)

        for note in ["oldest", "older", "recent", "newest"]:
            store.append("shared", note)

        assert store.read("shared") == "oldest\n\nolder\n\nrecent\n\nnewest"
        assert provider.calls == []

    def test_closing_folds_the_overflow_into_one_summary(self, tmp_path):
        """One provider call, carrying only what overflowed, and the kept
        entries survive it word for word."""
        provider = MockProvider(responses=["a compact recap"])
        store = SummarizingMemory(tmp_path, provider, "test-model", keep=2)

        for note in ["oldest", "older", "recent", "newest"]:
            store.append("shared", note)
        store.close()

        assert store.read("shared") == (
            f"{CONSOLIDATED_PREFIX}\na compact recap\n\nrecent\n\nnewest"
        )
        assert len(provider.calls) == 1
        assert provider.calls[0]["messages"][-1]["content"] == "oldest\n\nolder"

    def test_the_scope_is_a_key_and_the_file_is_under_the_root(self, tmp_path):
        """A store handed a scope that reads like a path must not follow it:
        the encoding leaves one filename, and the workspace confines it."""
        store = SummarizingMemory(tmp_path, MockProvider(), "test-model")

        assert store.path("shared") == tmp_path / "shared.json"
        assert store.path("../../elsewhere") == (
            tmp_path / "%2E%2E%2F%2E%2E%2Felsewhere.json"
        )
        assert store.age("shared") is None
        store.append("shared", "a note")
        assert store.age("shared") == 0

    @pytest.mark.parametrize("subclassed", [False, True])
    def test_a_session_can_be_told_to_keep_its_memory_in_one(self, tmp_path, subclassed):
        """The whole point of the slot: the session addresses it by scope and
        never learns it is not a file of prose."""
        class CustomSummarizing(SummarizingMemory):
            pass

        summarizer = MockProvider(responses=["Kept recap."])
        store_type = CustomSummarizing if subclassed else SummarizingMemory
        store = store_type(tmp_path, summarizer, "test-model", keep=0)
        session = Session(
            gameplan="debate",
            topic="T",
            provider=MockProvider(responses=["END_SESSION", "Summary."]),
            turn_delay_sec=0,
            memory="the-session",
            memory_store=store,
            access_policy=AccessPolicy(workspace=tmp_path),
        )

        session.memory.append("something worth keeping")
        assert session.memory.read() == "something worth keeping"
        assert session.memory.path == tmp_path / "the-session.json"
        session.add_agent("Alice", model="m")
        session.add_agent("Bob", model="m")
        session.add_agent("Mod", model="m", role="orchestrator")
        session.run()
        assert len(summarizer.calls) == 1
        assert session.memory.read() == f"{CONSOLIDATED_PREFIX}\nKept recap."


class TestCuratedMemory:
    """The third bundled store: a ceiling the agents are held to.

    What the ceiling *does* is decided in the crate and asserted there. What
    is left for this suite is the constructor's own keyword and the two
    directions the store can be handed across the boundary.
    """

    def test_the_ceiling_is_the_crate_default_or_the_keyword_that_overrides_it(
        self, tmp_path
    ):
        """The default is written into the pyo3 signature rather than repeated
        in Python, so a caller omitting it and one naming it both have to come
        back with the figure the crate holds."""
        assert CuratedMemory(tmp_path).budget() == DEFAULT_MEMORY_BUDGET
        assert CuratedMemory(tmp_path, budget=40).budget() == 40

    def test_a_session_can_be_told_to_keep_its_memory_in_one(self, tmp_path):
        """The slot again, with the store that has a ceiling: the session
        addresses it by scope and reads back what the prompt would quote."""
        store = CuratedMemory(tmp_path)
        session = Session(
            gameplan="debate",
            topic="T",
            memory="the-session",
            memory_store=store,
            access_policy=AccessPolicy(workspace=tmp_path),
        )

        session.memory.append("something worth keeping")
        assert session.memory.read().endswith("something worth keeping")
        assert session.memory.path == tmp_path / "the-session.md"

    def test_a_store_written_in_python_is_asked_for_its_ceiling(self, tmp_path):
        """``budget`` is what decides whether the session offers ``edit_memory``
        at all, so a store subclassed in Python has to be asked — the answer
        crosses back as ``None`` or an ``int``."""

        class Counted(MemoryStore):
            def __init__(self):
                self.asked = 0
                self.notes = []

            def read(self, scope):
                return "\n".join(self.notes)

            def append(self, scope, note):
                self.notes.append(note)

            def budget(self):
                self.asked += 1
                return 600

        store = Counted()
        session = Session(
            gameplan="debate",
            topic="T",
            provider=MockProvider(responses=["END_SESSION"]),
            turn_delay_sec=0,
            memory="the-session",
            memory_store=store,
            memory_write=True,
            access_policy=AccessPolicy(workspace=tmp_path),
        )
        session.add_agent("Alice", model="m")
        session.add_agent("Bob", model="m")
        session.add_agent("Mod", model="m", role="orchestrator")
        session.run()

        assert store.asked > 0
