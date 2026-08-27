"""Tests for kerness.utils."""

import pytest

from kerness.utils import (
    parse_memory_markers,
    parse_orchestrator_call,
    parse_session_end,
    retry,
)


class TestParseOrchestratorCall:
    @pytest.mark.parametrize("text, names, expected", [
        ("@Alice, present your argument.", ["Alice", "Bob"],
         ("Alice", "present your argument.")),
        ("@Alice: please respond.", ["Alice"], ("Alice", "please respond.")),
        # A name with punctuation and a space in it still resolves whole.
        ("@Dr. Chen, share your findings.", ["Dr. Chen", "Prof. Smith"],
         ("Dr. Chen", "share your findings.")),
        ("@Alice", ["Alice"], ("Alice", "")),
        ("Let me think about this.", ["Alice", "Bob"], None),
        # Only registered participants are routable.
        ("@Charlie, your turn.", ["Alice", "Bob"], None),
    ])
    def test_routing(self, text, names, expected):
        assert parse_orchestrator_call(text, names) == expected


class TestParseSessionEnd:
    @pytest.mark.parametrize("text, expected", [
        ("That concludes. END_SESSION", "END_SESSION"),
        ("All agree. CONSENSUS_REACHED", "CONSENSUS_REACHED"),
        ("end_session", "END_SESSION"),
        ("CONSENSUS_REACHED and END_SESSION", "CONSENSUS_REACHED"),
        ("Just a normal response.", None),
        # A keyword embedded in a longer token is not a keyword.
        ("NOT_END_SESSION_YET", None),
    ])
    def test_defaults(self, text, expected):
        assert parse_session_end(text) == expected


class TestParseMemoryMarkers:
    @pytest.mark.parametrize("text, cleaned, notes", [
        ("Just a normal response.", "Just a normal response.", []),
        ("I think X is true.\n@MEMORY: X is confirmed.",
         "I think X is true.", ["X is confirmed."]),
        ("@MEMORY: note one\nSome text.\n@MEMORY: note two",
         "Some text.", ["note one", "note two"]),
        ("Hello.\n@memory: lowercase marker", "Hello.", ["lowercase marker"]),
        # A marker with nothing after it is a marker with nothing to save.
        ("Hello.\n@MEMORY:", "Hello.", []),
        ("@MEMORY: only notes", "", ["only notes"]),
    ])
    def test_extraction(self, text, cleaned, notes):
        assert parse_memory_markers(text) == (cleaned, notes)


class TestRetry:
    def test_success_first_attempt(self):
        assert retry(lambda: 42, retries=2, backoff_sec=0) == 42

    @pytest.mark.parametrize("interval", [{}, {"interval_sec": 0}])
    def test_fails_then_succeeds(self, interval):
        """Both waiting strategies recover; a fixed interval is not a separate
        code path for the caller, only for the sleep between attempts."""
        calls = {"count": 0}

        def flaky():
            calls["count"] += 1
            if calls["count"] < 2:
                raise ValueError("fail")
            return "ok"

        assert retry(flaky, retries=2, backoff_sec=0, **interval) == "ok"
        assert calls["count"] == 2

    def test_exhausted_raises(self):
        def always_fail():
            raise ValueError("nope")

        with pytest.raises(ValueError, match="nope"):
            retry(always_fail, retries=1, backoff_sec=0)
