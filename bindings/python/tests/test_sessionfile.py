"""Tests for the on-disk state of one run.

The file exists so an interrupted session does not throw away provider calls
already paid for. That makes its failure modes expensive in a specific way:
a snapshot that loses part of the record, a stale file spliced into an
unrelated run, or a half-written file left where a resumable one belongs.
"""

import json

import pytest

from kerness.conversation import Conversation, Turn
from kerness.exceptions import SessionError
from kerness.sessionfile import (
    SCHEMA_VERSION,
    SessionSnapshot,
    check_identity,
    identity_for,
    load_snapshot,
    save_snapshot,
)

IDENTITY = identity_for(
    gameplan="debate", topic="Test", participants=["Alice", "Bob"],
    orchestrator="Mod",
)


class TestRoundTrip:
    def test_every_kind_of_record_survives(self, tmp_path):
        """``Conversation`` keeps directives, agent turns, and system notes in
        different places — turns only, both, and transcript only — and
        ``render()`` folds speaker and msg_type into a string. A snapshot that
        round-trips one kind proves nothing about the others, and one that
        persists the rendered form resumes a session that cannot tell an
        agent turn from a directive. ``SessionResult.history`` is rebuilt from
        the transcript, so a dropped msg_type reclassifies the closing summary.
        The loop state is what stops a resume from replaying a whole phase."""
        conversation = Conversation()
        conversation.directive("The topic")
        conversation.say("Alice", "My position", round_idx=1)
        conversation.note("something happened")
        conversation.say("Mod", "done", round_idx=4, msg_type="final_summary")

        turns = [*conversation.turns(),
                 Turn(role="assistant", speaker="Alice", content="hi",
                      round_idx=7, msg_type="final_summary")]
        loop = {"turn_count": 5, "phases": {"index": 1, "pending": ["Bob"]}}

        path = tmp_path / "run.json"
        save_snapshot(path, SessionSnapshot(
            identity=IDENTITY,
            turns=turns,
            transcript=conversation.transcript(),
            loop=loop,
            compactions=3,
        ))
        loaded = load_snapshot(path)

        assert loaded.turns == turns
        assert loaded.transcript == conversation.transcript()
        assert loaded.loop == loop
        assert loaded.compactions == 3

    def test_it_writes_readable_json(self, tmp_path):
        """The file is meant to be inspectable when a resume goes wrong."""
        path = tmp_path / "run.json"
        save_snapshot(path, SessionSnapshot(identity=IDENTITY))

        payload = json.loads(path.read_text(encoding="utf-8"))
        assert payload["version"] == SCHEMA_VERSION
        assert payload["identity"]["topic"] == "Test"


class TestMissingAndMalformed:
    def test_a_missing_file_is_not_an_error_and_is_not_created(self, tmp_path):
        """It just means this is the first run. Creating it here would leave a
        trace on disk for a session that had nothing to say yet."""
        path = tmp_path / "absent.json"

        assert load_snapshot(path) is None
        assert not path.exists()

    def test_an_unknown_schema_version_is_refused_naming_both(self, tmp_path):
        """Missing fields would otherwise resume as silent defaults — a phase
        pointer of zero reads as 'start over', not as 'unknown' — and a caller
        cannot act on the refusal without knowing what this build speaks."""
        path = tmp_path / "run.json"
        path.write_text(json.dumps({"version": 999}), encoding="utf-8")

        with pytest.raises(SessionError) as exc:
            load_snapshot(path)

        assert "999" in str(exc.value)
        assert str(SCHEMA_VERSION) in str(exc.value)

    def test_unparseable_json_names_the_file(self, tmp_path):
        """A truncated file should say which one, not raise JSONDecodeError
        from somewhere inside a run."""
        path = tmp_path / "run.json"
        path.write_text("{not json", encoding="utf-8")

        with pytest.raises(SessionError, match="run.json"):
            load_snapshot(path)

    def test_a_save_lands_at_the_path_it_was_given_and_leaves_nothing_else(
        self, tmp_path
    ):
        """The rename is what makes a save atomic; a leftover .tmp would mean
        it did not happen. Missing parents are the caller's layout, not an
        error to raise mid-run."""
        nested = tmp_path / "nested" / "deeper" / "run.json"
        save_snapshot(nested, SessionSnapshot(identity=IDENTITY))
        assert nested.exists()

        flat = tmp_path / "run.json"
        save_snapshot(flat, SessionSnapshot(identity=IDENTITY))
        assert sorted(p.name for p in tmp_path.iterdir()) == ["nested", "run.json"]


class TestIdentity:
    def test_the_same_run_passes_however_it_was_registered(self):
        """Participants are sorted because add_agent order is not what
        makes two runs the same session. Refusing over it would be a false
        alarm on a script whose calls were merely reordered."""
        check_identity(IDENTITY, dict(IDENTITY))
        check_identity(IDENTITY, identity_for(
            gameplan="debate", topic="Test",
            participants=["Bob", "Alice"], orchestrator="Mod",
        ))

    @pytest.mark.parametrize("field,value", [
        ("gameplan", "research"),
        ("topic", "Something else"),
        ("orchestrator", "Other"),
        ("participants", ["Alice", "Carol"]),
    ])
    def test_a_mismatch_is_refused_named_and_recoverable(self, field, value):
        """Resume is automatic, so this check is the only thing between a
        stale run.json and a session that silently inherits an unrelated
        conversation. 'Cannot resume' alone would say neither which field
        differs nor what to do about it."""
        current = dict(IDENTITY, **{field: value})

        with pytest.raises(SessionError) as exc:
            check_identity(IDENTITY, current)

        assert field in str(exc.value)
        assert "Delete the file" in str(exc.value)
