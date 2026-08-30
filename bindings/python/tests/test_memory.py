"""Tests for kerness.memory."""

from kerness.memory import Memory


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
