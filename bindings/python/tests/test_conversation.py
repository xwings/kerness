"""Tests for kerness.conversation."""

from kerness.conversation import Conversation, Message, Turn, render_turn


class TestRendering:
    def test_agents_are_prefixed_and_directives_are_not(self):
        """The rendered shape is frozen — the session suite asserts against it.
        A directive is the harness talking, so a speaker prefix on it would
        read as one more participant in the room."""
        assert render_turn(
            Turn(role="assistant", speaker="Alice", content="hello")
        ) == {"role": "assistant", "content": "[Alice] hello"}

        convo = Conversation()
        convo.directive("topic")
        convo.say("Mod", "@Alice, go.", 1, "orchestrator")
        convo.say("Alice", "My view.", 2, "turn")
        assert convo.render() == [
            {"role": "user", "content": "topic"},
            {"role": "assistant", "content": "[Mod] @Alice, go."},
            {"role": "assistant", "content": "[Alice] My view."},
        ]

    def test_both_accessors_hand_back_a_fresh_list(self):
        """Callers pass the render straight to a provider and the transcript
        straight into SessionResult; either one aliasing internal state lets a
        caller edit the record of a run in place."""
        convo = Conversation()
        convo.say("Alice", "hi")

        convo.render().append({"role": "user", "content": "injected"})
        convo.transcript().clear()

        assert len(convo.render()) == 1
        assert len(convo.transcript()) == 1


class TestTranscript:
    def test_it_holds_what_was_said_and_noted_but_not_what_was_directed(self):
        """A directive is read by models but authored by no one; a note is
        authored for the caller and never shown to a model. The two therefore
        land in different places, and the round and type are what let a caller
        tell the closing summary apart from an ordinary turn."""
        convo = Conversation()
        convo.directive("topic")
        convo.say("Alice", "My view.", 1)
        convo.note("Session ended: END_SESSION")
        convo.say("Mod", "Summary.", 7, "final_summary")

        assert [m.sender for m in convo.transcript()] == ["Alice", "system", "Mod"]
        assert convo.transcript()[1] == Message(
            sender="system", content="Session ended: END_SESSION", msg_type="system"
        )
        assert convo.transcript()[2].round_idx == 7
        assert convo.transcript()[2].msg_type == "final_summary"

        # The note is for the caller; only the directive and the two turns are
        # anything a model sees.
        assert len(convo.render()) == 3


class TestRaw:
    def test_it_passes_content_through_and_claims_no_speaker(self):
        """A tool message's prefix is content, not a speaker — and nobody said
        it, so it belongs in the conversation but not in the record of who
        contributed what."""
        convo = Conversation()
        convo.raw("assistant", "[Tool:cmd] hi")

        assert convo.render() == [{"role": "assistant", "content": "[Tool:cmd] hi"}]
        assert convo.transcript() == []
