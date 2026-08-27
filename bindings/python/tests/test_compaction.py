"""Tests for conversation compaction.

The defect this subsystem exists to prevent is a session dying at the provider
with a context-length error, deep into a run the caller has already paid for.
The defects these tests exist to prevent are the ways compaction could make
that worse: losing the topic, losing the newest turn, thrashing, or trading a
real conversation for a summary that never arrived.
"""

from kerness.compaction import (
    COMPACT_TO_FRACTION,
    SUMMARY_PREFIX,
    compact,
    estimate_tokens,
    estimate_turns,
    summary_request,
)
from kerness.conversation import Turn


def _turn(speaker, content, role="assistant"):
    return Turn(role=role, speaker=speaker, content=content)


def _conversation(n, size=400):
    """A topic directive followed by *n* assistant turns of *size* chars."""
    turns = [Turn(role="user", speaker="", content="The topic under debate")]
    for i in range(n):
        turns.append(_turn(f"A{i}", f"turn {i} " + "x" * size))
    return turns


class TestEstimates:
    def test_it_counts_the_rendered_form(self):
        """A speaker's name is prefixed on the way to a provider, so counting
        the raw content under-reports every assistant turn in the session. A
        directive has no speaker, so nothing is added to it."""
        assert estimate_turns([_turn("Alice", "hello")]) == estimate_tokens(
            "[Alice] hello"
        )
        assert estimate_turns(
            [Turn(role="user", speaker="", content="hello there")]
        ) == estimate_tokens("hello there")


class TestUnderTheLimit:
    def test_it_leaves_a_short_conversation_alone(self):
        """``None`` and "a copy of the same turns" are not interchangeable —
        the caller uses the difference to decide whether to save."""
        turns = _conversation(3)
        assert compact(turns, limit=10_000, summarize=_unused) is None

    def test_a_single_oversized_turn_is_not_compactable(self):
        """There is nothing to summarize that would not be the whole
        conversation, and returning a summary of everything would leave the
        next speaker with no actual turn to reply to."""
        turns = [Turn(role="user", speaker="", content="x" * 100_000)]
        assert compact(turns, limit=10, summarize=_unused) is None


class TestOverTheLimit:
    def test_the_result_is_topic_summary_then_recent_turns(self):
        """Every later turn is a reply to the topic, so dropping it would leave
        a summary and some recent turns discussing something invisible. The
        summary itself is a directive, not a speaker: a framework-written recap
        attributed to an agent would be a sentence that agent never said, quoted
        back to it as its own. And what is kept is kept byte for byte — a
        paraphrased recent turn would be the model reading words its author did
        not use."""
        turns = _conversation(40)
        result = compact(turns, limit=200, summarize=lambda t: "Alice argued X")

        assert result is not None
        assert len(result) < len(turns)

        assert result[0] == turns[0]

        assert result[1].role == "user"
        assert result[1].speaker == ""
        assert SUMMARY_PREFIX in result[1].content
        assert "Alice argued X" in result[1].content

        assert result[-1] == turns[-1]

    def test_the_newest_turn_is_kept_even_when_it_alone_is_too_big(self):
        """Otherwise the turn that prompted this call gets summarized away and
        the next speaker replies to a recap of the question."""
        turns = [
            Turn(role="user", speaker="", content="topic"),
            _turn("A", "x" * 200),
            _turn("B", "y" * 100_000),
        ]
        result = compact(turns, limit=100, summarize=lambda t: "s")

        assert result is not None
        assert result[-1] == turns[-1]

    def test_the_result_leaves_room_for_turns_to_come(self):
        """The anti-thrash property, and the reason COMPACT_TO_FRACTION is
        below 1. Compacting to exactly the limit leaves the conversation one
        turn from breaching again — a session paying for a summary every turn
        while losing history each time. So neither the compacted conversation
        nor that conversation plus another turn needs compacting."""
        limit = 400
        result = compact(_conversation(60), limit=limit,
                         summarize=lambda t: "short summary")

        assert estimate_turns(result) < limit * COMPACT_TO_FRACTION * 1.5
        assert compact(result, limit=limit, summarize=_unused) is None

        grown = result + [_turn("Z", "z" * 200)]
        assert compact(grown, limit=limit, summarize=_unused) is None

    def test_the_summarizer_sees_only_the_dropped_turns(self):
        """Summarizing turns that are also being kept verbatim pays a provider
        call to duplicate what the model can already read."""
        turns = _conversation(40)
        seen = []
        result = compact(turns, limit=200,
                         summarize=lambda t: seen.append(list(t)) or "s")

        dropped = seen[0]
        kept = result[2:]
        assert dropped
        assert not [t for t in dropped if t in kept]


class TestSummarizerFailure:
    def test_an_empty_summary_leaves_the_conversation_intact(self):
        """A failed provider call must not cost the caller their history. The
        session stays over the limit, which the provider will complain about —
        far better than turns silently replaced by nothing. Whitespace is a
        failure too: it is what a model returns when it had nothing to say."""
        assert compact(_conversation(40), limit=200,
                       summarize=lambda t: "") is None
        assert compact(_conversation(40), limit=200,
                       summarize=lambda t: "   \n  ") is None


class TestSummaryRequest:
    def test_it_asks_without_offering_tools_or_a_persona(self):
        """A summarizer that carried the orchestrator's system prompt would
        try to route a turn instead of summarizing one."""
        messages = summary_request([_turn("Alice", "hello")])

        assert [m["role"] for m in messages] == ["system", "user"]
        assert "[Alice] hello" in messages[1]["content"]


def _unused(turns):
    raise AssertionError("summarize must not be called when nothing is dropped")
