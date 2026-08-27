"""Tests for kerness.loop — control flow and result shaping.

The loop reaches the session only through LoopHost, so these run with a
recording stub and no provider at all.
"""

from kerness.harness import LoopSpec, PhaseSpec, ResultField
from kerness.loop import (
    FORCED_END_NOTE,
    OrchestratorLoop,
    closing_prompt,
    parse_result_fields,
    strip_result_block,
    verdict_rethink_prompt,
)


class StubHost:
    """Replays canned orchestrator replies and records what the loop did."""

    def __init__(self, replies, participant_reply="Said something.",
                 closing_reply=None, closing_replies=None):
        self._replies = list(replies)
        self._participant_reply = participant_reply
        # When set, the closing turn answers with this instead of popping the
        # queue — so a test can oversupply routing replies without the leftovers
        # standing in for the summary.
        self._closing_reply = closing_reply
        # An explicit script for the closing passes, in order. Only tests that
        # care about the draft-vs-committed distinction need it.
        self._closing_replies = list(closing_replies or [])
        self._last_closing: str | None = None
        self.delivered: list[tuple[str, str, str]] = []
        self.notes: list[str] = []
        self.directives: list[str] = []
        self.purposes: list[str] = []
        #: Every closing prompt in order — [0] is the draft ask, [-1] is the
        #: one whose answer was committed.
        self.closing_prompts: list[str] = []
        self.summary: str | None = None
        #: (name, instruction) for every routed turn — what a participant was
        #: actually told, which is where the phase contract either lands or
        #: does not.
        self.routed: list[tuple[str, str]] = []

    def orchestrator_turn(self, purpose):
        self.purposes.append(purpose)
        return self._replies.pop(0) if self._replies else "(exhausted)"

    def participant_turn(self, name, instruction):
        self.routed.append((name, instruction))
        return self._participant_reply

    def deliver(self, sender, text, turn, msg_type):
        self.delivered.append((sender, text, msg_type))

    def note(self, message):
        self.notes.append(message)

    def directive(self, text):
        self.directives.append(text)

    def closing_turn(self, prompt):
        self.closing_prompts.append(prompt)
        if self._closing_replies:
            reply = self._closing_replies.pop(0)
        elif self._closing_reply is not None:
            reply = self._closing_reply
        elif self._last_closing is not None:
            # The verdict rethink hands the draft back and asks for a revision.
            # A stub with nothing further to say stands by its draft, which is
            # what the prompt tells a real orchestrator to do when the draft
            # was already right. Returning "(exhausted)" here would make every
            # pre-rethink test fail on the stub's bookkeeping rather than on
            # anything the loop did.
            reply = self._last_closing
        else:
            reply = self._replies.pop(0) if self._replies else "Final summary."
        self._last_closing = reply
        return reply

    def record_summary(self, text, turn):
        self.summary = text


def loop(host, spec=None, **kwargs):
    return OrchestratorLoop(
        spec=spec or LoopSpec(),
        host=host,
        orchestrator_name="Mod",
        participant_names=["Alice", "Bob"],
        **kwargs,
    )


class TestTerminationComesFromTheHarness:
    """The milestone: keywords are gameplan data, not Python literals."""

    def test_the_declared_keyword_ends_it_and_the_old_literal_does_not(self):
        """Two halves of the same milestone. The gameplan's word has to work,
        and `END_SESSION` — the literal this replaced — must have no standing
        left over in a harness that named something else, or the removal was
        cosmetic."""
        spec = LoopSpec(terminate_on=("ALL_DONE",))

        declared = StubHost(["ALL_DONE", "Summary."])
        state = loop(declared, spec).run()
        assert state.end_reason == "keyword"
        assert "Session ended: ALL_DONE" in declared.notes

        undeclared = StubHost([
            "END_SESSION", "@Alice, go.", "ALL_DONE", "Summary."
        ])
        state = loop(undeclared, spec).run()
        assert "Session ended: ALL_DONE" in undeclared.notes
        assert state.turn_count > 2

    def test_only_the_consensus_keyword_sets_the_consensus_flag(self):
        """Both end the run; only one of them means the agents agreed."""
        spec = LoopSpec(terminate_on=("END_SESSION", "CONSENSUS_REACHED"))

        agreed = loop(StubHost(["CONSENSUS_REACHED", "Summary."]), spec).run()
        assert agreed.consensus_reached is True

        plain = loop(StubHost(["END_SESSION", "Summary."]), spec).run()
        assert plain.consensus_reached is False

    def test_the_hint_quotes_the_declared_keywords(self):
        host = StubHost(["mumble", "ALL_DONE", "Summary."])
        spec = LoopSpec(terminate_on=("ALL_DONE",))
        loop(host, spec).run()

        assert "ALL_DONE" in host.directives[0]


class TestRouting:
    def test_an_at_mention_routes_only_to_a_participant_the_loop_knows(self):
        """An unknown name has to fall into the retry path rather than route
        nowhere quietly — a loop that accepted `@Carol` would spend the turn
        and deliver nothing."""
        known = StubHost(["@Alice, open.", "END_SESSION", "Summary."])
        state = loop(known).run()
        assert [d[0] for d in known.delivered] == ["Mod", "Alice", "Mod"]
        assert state.turn_count == 3

        unknown = StubHost(["@Carol, open.", "END_SESSION", "Summary."])
        loop(unknown).run()
        assert unknown.directives, "an unroutable reply must be re-asked"


class TestRetries:
    def test_retries_exhaust_into_a_forced_end_and_zero_means_none(self):
        """A budget of zero has to mean no re-ask at all, not one. Off-by-one
        here is invisible in the outcome — both end forced — and shows up only
        as a wasted provider call."""
        exhausted = StubHost(["mumble", "mumble", "mumble", "Summary."])
        state = loop(exhausted, retries=2).run()
        assert state.end_reason == "forced"
        assert FORCED_END_NOTE in exhausted.notes

        none = StubHost(["mumble", "Summary."])
        assert loop(none, retries=0).run().end_reason == "forced"
        assert none.directives == []

    def test_a_recovering_retry_continues_the_session(self):
        """The mumbled turn is delivered like any other, so the retry path
        gets memory-marker processing too."""
        host = StubHost(["mumble", "@Alice, go.", "END_SESSION", "Summary."])
        state = loop(host, retries=2).run()

        assert state.end_reason != "forced"
        assert [d[0] for d in host.delivered] == ["Mod", "Mod", "Alice", "Mod"]

    def test_the_retry_budget_comes_from_the_harness(self):
        host = StubHost(["mumble"] * 6 + ["Summary."])
        spec = LoopSpec(orchestrator_retries=4)
        loop(host, spec).run()

        assert len(host.directives) == 4

    def test_hitting_max_turns_during_retries_is_not_a_forced_end(self):
        """The retry budget did not run out; the global turn budget did. Those
        outcomes must remain distinguishable in ``end_reason``.
        """
        host = StubHost(["mumble"] * 5, closing_reply="Summary.")
        state = loop(host, max_turns=1, retries=5).run()

        assert state.end_reason == "max_turns"
        assert FORCED_END_NOTE not in host.notes


class TestLimits:
    def test_max_turns_stops_a_loop_that_never_ends(self):
        """From either source, with the caller's value outranking the harness.
        The orchestrator turn and the participant turn it routes to are two
        turns, so an odd limit landing between them stops before the second
        rather than overshooting."""
        host = StubHost(["@Alice, go."] * 20)
        state = loop(host, max_turns=6).run()
        assert state.turn_count <= 6
        assert state.end_reason == "max_turns"
        assert host.summary is not None, "the limit must not skip the closing turn"

        harness = loop(StubHost(["@Alice, go."] * 20), LoopSpec(max_turns=4))
        assert harness.run().turn_count <= 4

        override = loop(
            StubHost(["@Alice, go."] * 20), LoopSpec(max_turns=40), max_turns=2
        )
        assert override.run().turn_count <= 2

        assert loop(
            StubHost(["@Alice, go."] * 20), max_turns=3
        ).run().turn_count == 3

    def test_retries_answer_to_max_turns_too(self):
        """A retry spends a turn like any other."""
        host = StubHost(["mumble"] * 20)
        assert loop(host, max_turns=2, retries=5).run().turn_count == 2


class TestClosingPrompt:
    def test_declared_fields_are_named_in_the_prompt_and_nothing_else_is(self):
        assert "json" not in closing_prompt(())

        prompt = closing_prompt((
            ResultField("consensus", "bool", "Whether they agreed."),
            ResultField("summary", "str"),
        ))
        assert '"consensus": <bool>' in prompt
        assert "Whether they agreed." in prompt

    def test_the_loop_uses_the_declared_shape(self):
        host = StubHost(["END_SESSION", "Done."])
        OrchestratorLoop(
            spec=LoopSpec(), host=host, orchestrator_name="Mod",
            participant_names=["Alice"],
            result_fields=(ResultField("verdict", "str"),),
        ).run()

        assert '"verdict"' in host.closing_prompts[0]


class TestResultParsing:
    FIELDS = (
        ResultField("consensus", "bool"),
        ResultField("summary", "str"),
        ResultField("points", "list"),
        ResultField("score", "int"),
    )

    def test_the_object_is_read_fenced_or_bare(self):
        """A model that forgets the fence still wrote the answer."""
        fenced = (
            'They agreed.\n\n```json\n{"consensus": true, "summary": "Agreed.",'
            ' "points": ["a", "b"], "score": 7}\n```'
        )
        assert parse_result_fields(fenced, self.FIELDS) == {
            "consensus": True, "summary": "Agreed.",
            "points": ["a", "b"], "score": 7,
        }

        bare = 'Summary.\n\n{"consensus": false, "summary": "No."}'
        parsed = parse_result_fields(bare, self.FIELDS)
        assert parsed["consensus"] is False
        assert parsed["summary"] == "No."

    def test_the_declared_fields_alone_decide_what_comes_back(self):
        """Never a partial dict, and never an undeclared key. Callers should
        not have to guard each one, so a field the model omitted, a field it
        mangled, and a field nobody declared all resolve the same way — the
        closing turn is the last thing that happens, and failing it would
        discard a whole transcript over a formatting mistake. The mutable
        defaults are fresh each call, or one caller's `.append` shows up in the
        next parse."""
        DEFAULTS = {"consensus": False, "summary": "", "points": [], "score": 0}

        parsed = parse_result_fields('```json\n{"summary": "S"}\n```', self.FIELDS)
        assert parsed == {**DEFAULTS, "summary": "S"}

        assert parse_result_fields("nothing", self.FIELDS) == DEFAULTS
        assert parse_result_fields(
            "Summary.\n\n```json\n{not json\n```", self.FIELDS
        ) == DEFAULTS
        assert parse_result_fields(
            "nothing", (ResultField("score", "float"), ResultField("meta", "dict"))
        ) == {"score": 0.0, "meta": {}}
        assert parse_result_fields('```json\n{"a": 1}\n```', ()) == {}

        parsed["points"].append("mutated")
        assert parse_result_fields("nothing", self.FIELDS)["points"] == []

    def test_every_declared_type_and_alias_is_coerced(self):
        fields = (
            ResultField("string", "string"),
            ResultField("integer", "integer"),
            ResultField("number", "number"),
            ResultField("boolean", "boolean"),
            ResultField("mapping", "dict"),
        )
        text = (
            '```json\n{"string": 3, "integer": "4", "number": "2.5", '
            '"boolean": "yes", "mapping": {"ok": true}}\n```'
        )

        assert parse_result_fields(text, fields) == {
            "string": "3",
            "integer": 4,
            "number": 2.5,
            "boolean": True,
            "mapping": {"ok": True},
        }

        # A scalar where a list was declared is wrapped, not discarded.
        assert parse_result_fields(
            '```json\n{"points": "just one", "consensus": "yes"}\n```', self.FIELDS
        ) == {
            "points": ["just one"], "consensus": True, "summary": "", "score": 0,
        }


class TestSummaryText:
    def test_the_json_block_is_stripped_from_the_summary(self):
        text = 'They agreed.\n\n```json\n{"consensus": true}\n```'
        assert strip_result_block(text) == "They agreed."
        assert strip_result_block("Just prose.") == "Just prose."

    def test_the_loop_records_the_stripped_text(self):
        host = StubHost([
            "END_SESSION",
            'They agreed.\n\n```json\n{"consensus": true}\n```',
        ])
        state = OrchestratorLoop(
            spec=LoopSpec(), host=host, orchestrator_name="Mod",
            participant_names=["Alice"],
            result_fields=(ResultField("consensus", "bool"),),
        ).run()

        assert state.final_summary == "They agreed."
        assert state.fields == {"consensus": True}
        assert host.summary == "They agreed."


class TestNoOrchestrator:
    def test_a_headless_loop_takes_no_turns(self):
        """Nothing drives, so nothing runs — and no closing turn is attempted."""
        host = StubHost(["unused"])
        state = OrchestratorLoop(
            spec=LoopSpec(), host=host, orchestrator_name="",
            participant_names=["Alice"], max_turns=0,
        ).run()

        assert state.turn_count == 0
        assert host.summary is None


# --------------------------------------------------------------------------
# Phases
# --------------------------------------------------------------------------

THINK = PhaseSpec(name="think", rounds=1, instruction="State your own view.")
ARGUE = PhaseSpec(name="argue", rounds=2, instruction="Pick a side.")
RETHINK = PhaseSpec(
    name="rethink", rounds=1, rethink=True, instruction="Revisit your opening."
)


def phased(*phases, **kwargs):
    """A spec whose structure is the thing under test."""
    kwargs.setdefault("max_turns", 200)
    kwargs.setdefault("max_rounds", 10)
    return LoopSpec(phases=tuple(phases), **kwargs)


def routing(n):
    """*n* orchestrator replies that each call on someone, alternating."""
    names = ["Alice", "Bob"]
    return [f"@{names[i % 2]}, your turn." for i in range(n)]


class TestPhasesReachParticipants:
    """The phase contract has to arrive at the agent that must honour it.

    Relaying it through the orchestrator would not do: the phase would live in
    the orchestrator's system prompt as "tell participants this", and one
    forgetful routing turn would leave a participant answering with no idea
    which phase it was in.
    """

    def test_the_phase_rides_every_routed_turn_without_displacing_the_ask(self):
        """Composed, not substituted. Every routed turn has to carry the phase
        — one that slipped through unmarked is a participant answering blind —
        and it has to carry what the orchestrator actually asked for alongside,
        or the phase instruction has replaced the routing rather than joined
        it."""
        every = StubHost(routing(2) + ["END_SESSION"], closing_reply="Summary.")
        loop(every, phased(THINK, ARGUE)).run()

        assert len(every.routed) == 2
        for _, instruction in every.routed:
            assert "[Phase: think]" in instruction
            assert "State your own view." in instruction

        asked = StubHost(["@Alice, answer the cost question.", "END_SESSION", "S."])
        loop(asked, phased(THINK)).run()

        name, instruction = asked.routed[0]
        assert name == "Alice"
        assert "answer the cost question." in instruction
        assert "State your own view." in instruction

    def test_phases_arrive_in_declared_order(self):
        host = StubHost(routing(8) + ["END_SESSION"], closing_reply="Summary.")
        loop(host, phased(THINK, ARGUE, RETHINK)).run()

        seen = []
        for _, instruction in host.routed:
            for phase in ("think", "argue", "rethink"):
                if f"[Phase: {phase}]" in instruction and phase not in seen:
                    seen.append(phase)
        assert seen == ["think", "argue", "rethink"]

    def test_a_rethink_phase_says_so_in_the_turn_itself(self):
        """A ``[rethink]`` marker in the orchestrator's phase listing would
        leave the participant doing the rethinking untold. The instruction has
        to reach the turn itself."""
        host = StubHost(routing(2) + ["END_SESSION"], closing_reply="Summary.")
        loop(host, phased(RETHINK)).run()

        assert "rethink phase" in host.routed[0][1]
        assert "whether it changed" in host.routed[0][1]


class TestRoundsClose:
    """A round is every participant speaking once — not one turn, and not one
    orchestrator call."""

    def test_it_takes_the_last_straggler_and_not_a_repeat_to_close_one(self):
        """Counting turns instead of speakers would close a round on Alice
        talking twice, which is the reading that makes a round meaningless."""
        repeat = StubHost([
            "@Alice, go.", "@Alice, again.", "END_SESSION", "Summary.",
        ])
        assert loop(repeat, phased(THINK)).run().rounds_run == 0

        straggler = StubHost([
            "@Alice, go.", "@Alice, again.", "@Bob, go.",
            "END_SESSION", "Summary.",
        ])
        assert loop(straggler, phased(THINK, ARGUE)).run().rounds_run == 1

    def test_the_briefing_names_who_still_owes_a_turn_and_is_reissued(self):
        """The orchestrator cannot see the loop's pending set. Without the
        briefing it re-calls whoever it likes and the round never closes —
        which means a briefing sent once, at the start, is no better: the
        pending set it described is stale the moment the round turns over."""
        opening = StubHost(["@Alice, go.", "END_SESSION", "Summary."])
        loop(opening, phased(THINK)).run()

        assert opening.directives
        assert "Yet to speak this round: Alice, Bob." in opening.directives[0]

        turnover = StubHost([
            "@Alice, go.", "@Bob, go.", "END_SESSION", "Summary.",
        ])
        loop(turnover, phased(THINK, ARGUE)).run()

        assert len(turnover.directives) >= 2
        assert "argue" in turnover.directives[-1]


class TestPhasesEndTheRun:
    """Exhausting the declared structure is a normal ending: the agents ran the
    complete round, so the judge answers. ``terminate_on`` is the *early* exit,
    not the only one."""

    def test_the_last_phase_running_out_stops_the_loop_and_still_closes(self):
        host = StubHost(routing(20), closing_reply="Summary.")
        state = loop(host, phased(THINK)).run()

        assert state.end_reason == "phases_complete"
        assert host.summary == "Summary."
        assert state.final_summary == "Summary."

    def test_it_runs_the_declared_number_of_rounds_and_stops(self):
        """think(1) + argue(2) + rethink(1) = 4 rounds, 2 participants each."""
        host = StubHost(routing(40), closing_reply="Summary.")
        state = loop(host, phased(THINK, ARGUE, RETHINK)).run()

        assert state.rounds_run == 4
        assert len(host.routed) == 8
        assert state.phase_reached == "rethink"

    def test_a_terminator_still_exits_early(self):
        host = StubHost(["@Alice, go.", "END_SESSION", "Summary."])
        state = loop(host, phased(THINK, ARGUE, RETHINK)).run()

        assert state.end_reason == "keyword"
        assert state.rounds_run == 0


class TestAdvanceOnIsReadBack:
    """``advance_on`` was a dead round-trip: the orchestrator prompt said "write
    NEXT_PHASE", and a reply that did matched neither a terminator nor an
    ``@Name``, so it fell into the retry path and burned turns. The harness
    instructed the model to do something the loop punished."""

    def test_the_keyword_advances_the_phase_whether_or_not_it_routes(self):
        """Alone, it must move the pointer without being punished as an
        unroutable reply. Combined with an `@Name` in one breath, the routed
        turn has to land in the phase the same reply just advanced to — reading
        the pointer before applying the keyword sends it to the old one."""
        alone = StubHost([
            "NEXT_PHASE", "@Alice, go.", "END_SESSION", "Summary.",
        ])
        loop(alone, phased(THINK, ARGUE)).run()

        assert "[Phase: argue]" in alone.routed[0][1]
        assert FORCED_END_NOTE not in alone.notes
        assert not any("didn't contain an @Name" in d for d in alone.directives)

        combined = StubHost([
            "NEXT_PHASE. @Alice, go.", "END_SESSION", "Summary.",
        ])
        loop(combined, phased(THINK, ARGUE)).run()

        assert "[Phase: argue]" in combined.routed[0][1]

    def test_advancing_past_the_last_phase_ends_the_run(self):
        host = StubHost(["NEXT_PHASE", "Summary."])
        state = loop(host, phased(THINK)).run()

        assert state.end_reason == "phases_complete"
        assert host.summary == "Summary."

    def test_a_harness_that_declares_no_keyword_ignores_the_word(self):
        host = StubHost(["NEXT_PHASE", "@Alice, go.", "END_SESSION", "S."])
        loop(host, phased(THINK, ARGUE, advance_on="")).run()

        assert "[Phase: think]" in host.routed[0][1]


class TestMaxRoundsIsRealNow:
    def test_it_caps_a_single_phase(self):
        """A phase declaring more rounds than the harness allows runs the
        harness's number."""
        greedy = PhaseSpec(name="argue", rounds=99, instruction="Argue.")
        host = StubHost(routing(40), closing_reply="Summary.")
        state = loop(host, phased(greedy, max_rounds=2)).run()

        assert state.rounds_run == 2

    def test_it_is_not_a_total_across_phases(self):
        """Capping the total would let a harness stop before its rethink phase
        — destroying the one guarantee the phase list exists to give. debate
        declares max_rounds: 3 and phases summing to 5."""
        host = StubHost(routing(40), closing_reply="Summary.")
        state = loop(host, phased(THINK, ARGUE, RETHINK, max_rounds=3)).run()

        assert state.rounds_run == 4
        assert state.phase_reached == "rethink"

    def test_it_bounds_a_phase_less_harness(self):
        """With no phases the whole session is one implicit phase, which is the
        only configuration where max_rounds bounds the run."""
        host = StubHost(routing(40), closing_reply="Summary.")
        state = loop(host, LoopSpec(max_turns=200, max_rounds=3)).run()

        assert state.rounds_run == 3
        assert state.end_reason == "max_rounds"
        assert len(host.routed) == 6

    def test_max_turns_still_outranks_it(self):
        host = StubHost(routing(40), closing_reply="Summary.")
        state = loop(host, LoopSpec(max_turns=3, max_rounds=99)).run()

        assert state.turn_count <= 3
        assert state.end_reason == "max_turns"


class TestAPhaselessHarnessIsOtherwiseUntouched:
    """The tracker composes nothing and narrates nothing when a harness
    declared no structure. Only the round cap is new."""

    def test_nothing_is_briefed_composed_or_reported(self):
        host = StubHost(["@Alice, make the case.", "END_SESSION", "Summary."])
        state = loop(host, LoopSpec()).run()

        assert host.directives == []
        assert host.routed == [("Alice", "make the case.")]
        assert state.phase_reached == ""


class TestTheJudgeRethinksItsVerdict:
    """The orchestrator is also the judge, and a verdict written in one call
    and committed unread is the one turn nobody reviews. The phase list gives
    participants their rethink; this is the judge's."""

    DRAFT = "Alice won on the merits."
    FINAL = "Neither won outright; Bob conceded a point Alice never answered."

    def _run(self, spec=None, fields=(), closing_replies=None):
        host = StubHost(
            ["END_SESSION"],
            closing_replies=closing_replies or [self.DRAFT, self.FINAL],
        )
        state = OrchestratorLoop(
            spec=spec or LoopSpec(), host=host, orchestrator_name="Mod",
            participant_names=["Alice", "Bob"], result_fields=fields,
        ).run()
        return host, state

    def test_the_draft_is_revised_and_only_the_revision_is_kept(self):
        """The draft is embedded in the second prompt, not referred to: by the
        closing turn the judge's own reply may have fallen out of a truncated
        context, and "revise your summary" against a summary it cannot see just
        produces a second first draft. The draft itself is never delivered and
        never handed to record_summary — a session must not end with two
        summaries in its transcript, one superseded."""
        host, state = self._run()

        assert len(host.closing_prompts) == 2
        assert self.DRAFT in host.closing_prompts[1]
        assert "JSON" not in host.closing_prompts[1]  # no fields were declared

        assert state.final_summary == self.FINAL
        assert host.summary == self.FINAL
        assert all(self.DRAFT not in text for _, text, _ in host.delivered)

    def test_result_fields_are_asked_for_again_and_taken_from_the_second_pass(
        self,
    ):
        """The second prompt has to ask for the JSON again — a pass that only
        asked for prose would leave the fields to be scavenged from the draft —
        and the committed values have to be the revised ones. Reading them from
        the draft would leave the structured verdict contradicting the prose
        beside it, the worst of both passes."""
        host, state = self._run(
            fields=(ResultField("winner", "str"),),
            closing_replies=[
                'Alice won.\n\n```json\n{"winner": "Alice"}\n```',
                'On reflection, nobody did.\n\n```json\n{"winner": "nobody"}\n```',
            ],
        )

        assert "JSON" in host.closing_prompts[1]
        assert state.fields == {"winner": "nobody"}

    def test_it_is_on_by_default_and_a_harness_can_turn_it_off(self):
        """One extra provider call per session is a real cost, so a harness
        that does not want it says so — and the switch is only worth having
        because the default is the expensive one."""
        assert LoopSpec().verdict_rethink is True

        host, state = self._run(spec=LoopSpec(verdict_rethink=False))
        assert len(host.closing_prompts) == 1
        assert state.final_summary == self.DRAFT


class TestVerdictRethinkPrompt:
    def test_it_carries_the_draft_and_asks_for_exactly_what_is_declared(self):
        """A judge that replies with both versions, or with a critique of its
        own draft, hands the caller something no transcript reader wants — and
        asking for JSON from a harness that declared no fields invites a block
        nothing will ever parse."""
        prompt = verdict_rethink_prompt("They agreed on nothing.", ())

        assert "They agreed on nothing." in prompt
        assert "not both versions" in prompt
        assert "change their mind" in prompt
        assert "JSON" not in prompt

        assert "JSON" in verdict_rethink_prompt(
            "Draft.", (ResultField("verdict", "str"),)
        )
