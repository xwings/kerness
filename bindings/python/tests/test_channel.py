"""Tests for kerness.channel."""

import json
import tempfile
from pathlib import Path

from kerness.channel import (
    Channel,
    ConsoleChannel,
    FileChannel,
    LogChannel,
    MultiChannel,
)


class TestConsoleChannel:
    def test_agents_notices_and_a_custom_prefix_all_reach_stdout(self, capsys):
        ch = ConsoleChannel()
        ch.send("Alice", "Hello world")
        ch.send_system("Starting round 1")
        out = capsys.readouterr().out
        assert "[Alice] Hello world" in out
        assert "[System] Starting round 1" in out

        ConsoleChannel(prefix_format="<{sender}>").send("Bob", "Hi")
        assert "<Bob> Hi" in capsys.readouterr().out


class TestLogChannel:
    def test_one_file_holds_one_typed_event_per_line(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            ch = LogChannel(log_dir=tmpdir)
            ch.send("Alice", "hello")
            ch.send_system("system msg")
            files = list(Path(tmpdir).glob("session_*.jsonl"))
            assert len(files) == 1
            lines = files[0].read_text().strip().splitlines()
            assert len(lines) == 2
            event1 = json.loads(lines[0])
            assert event1["sender"] == "Alice"
            assert event1["content"] == "hello"
            assert event1["role"] == "assistant"
            assert "ts" in event1
            event2 = json.loads(lines[1])
            assert event2["sender"] == "system"
            assert event2["role"] == "system"


class TestMultiChannel:
    def test_delegates_to_all(self):
        from tests.conftest import CaptureChannel

        ch1 = CaptureChannel()
        ch2 = CaptureChannel()
        multi = MultiChannel(ch1, ch2)
        multi.send("Alice", "msg")
        multi.send_system("sys")
        assert len(ch1.messages) == 2
        assert len(ch2.messages) == 2
        assert ch1.messages[0]["sender"] == "Alice"
        assert ch2.messages[1]["type"] == "system"

    def test_a_failing_channel_is_logged_and_does_not_starve_the_others(
        self, caplog
    ):
        """A blip on a caller's remote channel must not cost the session its
        local transcript, but swallowing it silently would make the missing
        output look like the session never produced it."""
        from tests.conftest import CaptureChannel

        class BrokenChannel(Channel):
            def send(self, sender, message):
                raise RuntimeError("network down")

            def send_system(self, message):
                raise RuntimeError("network down")

        good = CaptureChannel()
        multi = MultiChannel(BrokenChannel(), good)
        multi.send("Alice", "msg")
        multi.send_system("sys")

        assert len(good.messages) == 2
        assert "BrokenChannel" in caplog.text


class TestFileChannel:
    def test_it_creates_the_file_and_appends_a_line_per_message(self):
        """Opening in write mode instead of append would leave the file holding
        only the last thing said."""
        with tempfile.TemporaryDirectory() as tmpdir:
            filepath = Path(tmpdir) / "output.txt"
            assert not filepath.exists()

            ch = FileChannel(filepath)
            ch.send("Alice", "first message")
            ch.send("Bob", "second message")
            ch.send_system("done")

            content = filepath.read_text()
            assert "[Alice] first message\n" in content
            assert "[Bob] second message\n" in content
            assert "[System] done\n" in content
            assert len(content.strip().splitlines()) == 3
