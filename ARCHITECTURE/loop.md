---
eatmycode_version: "1.1.0"
---

# Orchestrator Loop

## Goal

Own the harness's routing, phase progress, retry allowance, and closing verdict
as a resumable action machine. The loop decides *what happens next* — an
orchestrator turn, a participant turn, a delivery, a directive, a note, the
closing summary, or completion — and hands that decision to whoever is driving
it. It performs no provider calls, filesystem operations, or channel writes
itself: [run.md](run.md) executes each action in owned mode, and the blocking
`LoopHost` adapter does the same for a caller that drives the loop directly.
This supplies M2's scheduling and host-driven phase progression.

It does not own the contract it is bounded by ([harness.md](harness.md)), the
text scans it routes on ([utils.md](utils.md)), or the conversation it
schedules turns into ([conversation.md](conversation.md)).

## Status

`done`. `cargo test -p kerness --lib orchestrator` passes 50 tests;
`bindings/python/tests/test_loop.py` passes 40; the six
`TestResumingFromASessionFile` cases in `bindings/python/tests/test_session.py`
pass; the loop-facing integration tests in `crates/kerness/tests/session_run.rs`
pass as part of that file's 24.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/orchestrator.rs` | actions, loop state, the phase tracker, the blocking `LoopHost` driver, the closing prompts, and result parsing |
| `bindings/python/src/runtime.rs` | `PyLoopState` (`bindings/python/src/runtime.rs:522`) and `PyOrchestratorLoop` (`:649`), the standalone loop seen from Python with a Python `LoopHost` |
| `bindings/python/kerness/loop.py` | re-export shim |

## Language and Conventions

Rust crate module with one PyO3 binding module and a shim. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply; local facts:

- The saved scheduler is `#[serde(deny_unknown_fields)]`
  (`crates/kerness/src/orchestrator.rs:427`); `LoopAction` and `LoopStage`
  are internally tagged enums (`tag = "action"`, `tag = "stage"`, `:387`,
  `:417`) and `EndReason`/`LoopTurnKind` are `rename_all = "snake_case"`.
  Those tags are the `loop.scheduler` checkpoint contract
  ([sessionfile.md](sessionfile.md)); renaming a variant is a schema change.
- Counters are `i64` and advance with `saturating_add`
  (`crates/kerness/src/orchestrator.rs:620`, `:855`), so a
  hand-edited snapshot cannot overflow one back into a fresh allowance.
- `unreachable!` names its reason at `crates/kerness/src/orchestrator.rs:641`:
  `next_action` has already refused
  a completed loop before `submit_reply` matches on the stage.
- The fence regex is a `LazyLock<Regex>` with `.expect("static pattern")`
  (`crates/kerness/src/orchestrator.rs:38`).
- The Python constructor carries `#[allow(clippy::too_many_arguments)]`
  (`bindings/python/src/runtime.rs:669`) for its keyword-only signature; a
  Python host is any object with the `LoopHost` method names, called by
  `call_method1` and converted through `Catch`
  (`bindings/python/src/runtime.rs:583`), and `record_position` failures are
  deliberately dropped there (`:640`).
- Unit tests are inline: a `StubHost` (`crates/kerness/src/orchestrator.rs:1145`)
  replays scripted orchestrator replies and records deliveries and
  checkpoints; `driver` (`:1252`) builds a
  loop for three fixed participants. The Python tests mirror that with their
  own `StubHost` (`bindings/python/tests/test_loop.py:18`) and no provider.

## Design and Invariants

### Decomposition and dependency direction

Three pieces in one file. `PhaseTracker` (`crates/kerness/src/orchestrator.rs:154`)
owns the round counter and the phase pointer. `OrchestratorLoop` (`:436`) owns
the stage, the queue of pending actions, the `LoopState`, and the raw closing
reply. The free functions at the bottom — `closing_prompt` (`:935`),
`verdict_rethink_prompt` (`:965`), `parse_result_fields` (`:992`),
`strip_result_block` (`:1011`) — are pure text. The module imports `error`,
`harness`, `pyfmt`, and `utils`, and nothing above them; `session/run.rs`
imports it (`crates/kerness/src/session/run.rs:20`) and `session.rs` builds
it at preparation (`crates/kerness/src/session.rs:1116`).

### One action machine, two drivers

`next_action` (`crates/kerness/src/orchestrator.rs:567`) exposes the next
`LoopAction` without consuming it.
`submit_reply` (`:613`) consumes a requested turn; `acknowledge` (`:649`)
consumes a queued callback. The blocking `run` (`:510`) is a loop over exactly
those three calls against a `LoopHost`, so the normal path and the owned
[run.md](run.md) path cannot drift apart: `SessionRun` calls the same
`next_action` (`crates/kerness/src/session/run.rs:592`), `submit_reply`
(`:661`) and `acknowledge` (`:615`).

An orchestrator response (`accept_orchestrator`,
`crates/kerness/src/orchestrator.rs:854`) updates the turn
count, checks termination (`:856`), reads `advance_on` back (`:857`), queues
its delivery, and then either routes (`:879`), re-asks with a hint (`:891`),
or forces the end (`:896`). A participant response updates the pending set and
round count through `PhaseTracker::record_turn` (`:279`) before queuing its
delivery. The next participant request is already selected in the saved
scheduler state, so restoring it does not ask the orchestrator to route that
completed response again.

### Scheduling and checkpoints

`acknowledge` consumes a callback before the host applies it. The blocking
adapter publishes that consumed state through `record_position`
(`LoopHost::record_position`, `crates/kerness/src/orchestrator.rs:84`) before calling `deliver`, so a host that
saves inside delivery writes the matching transcript and loop position. The
owned run checkpoints its conversation and consumed callback together. External
channel delivery is not a transaction with that checkpoint.

A closing draft is stored before a rethink is requested
(`LoopStage::ClosingFinal { draft }`, `crates/kerness/src/orchestrator.rs:629`). Only the final pass queues a
summary and supplies result fields (`:632`). The complete stage stays terminal
under explicit stepping; the blocking adapter alone permits a fully completed
saved run to continue with a larger configured budget (`:514`), while
interrupted continuations keep their exact pending action.

### Restore validates before it trusts

`initialize_mode` (`crates/kerness/src/orchestrator.rs:738`) runs once, on the first `next_action`,
`host_limit_reached`, or `commit_host_turn`. Snapshots with only version-1
`turn_count` and `phases` use the turn-boundary continuation (`:782`); a
`scheduler` object is deserialized under `deny_unknown_fields` (`:749`) and
then rejected if its counters disagree with the outer ones, an active turn is
already past the configured allowance, a retry count is outside its budget, or
a `Turn`/`Complete` action sits in the pending queue (`:751`). A pending
participant who left the roster is refused by name (`:771`). Negative progress
is refused before either path (`:743`). `PhaseTracker::restore` (`:322`) clamps
a saved index to the current phase list (`:325`) and drops pending names no
longer on the roster (`:329`), because the phase list is rebuilt from the
harness rather than restored and a gameplan edited between runs can leave a
saved index past the end.

An immediate `snapshot` of a restored loop returns the supplied continuation
unchanged (`crates/kerness/src/orchestrator.rs:712`); successful initialization clears the resume map (`:792`),
leaving the owned scheduler state.

### Phase reuse without an automatic orchestrator

A host-driven session uses `host_instruction`
(`crates/kerness/src/orchestrator.rs:669`), `host_briefing` (`:673`),
`commit_host_turn` (`:679`), and `host_limit_reached` (`:700`). These
use the same phase tracker and turn ceiling but generate no automatic turns or
callbacks. Unknown participants and turns past the bound are errors. The
session's configured mode decides which interface drives it
(`crates/kerness/src/session/run.rs:658` against `:661`).

Every scheduled orchestrator turn receives the current pending roster
(`standing_briefing`, `crates/kerness/src/orchestrator.rs:813`), and every participant receives the current
phase requirement (`turn_instruction`, `:904`). Boundary directives are
additional shared context; they do not substitute for the per-turn briefing.
Repeated calls on an already-heard participant do not close a round while
another participant is still owed a turn.

### Invariants a change must preserve

- Termination comes only from `terminate_on`; `FORCED_END_NOTE`
  (`crates/kerness/src/orchestrator.rs:29`) is a
  message to a human, not a keyword. Enforced by
  `the_declared_keyword_ends_it_and_an_undeclared_one_does_not` (`:1315`) and
  `only_the_declared_terminator_ends_the_session`
  (`crates/kerness/tests/harness_contract.rs:518`).
- `max_turns` outranks every other bound, including retries and `max_rounds`
  (`crates/kerness/src/orchestrator.rs:847`, `:875`). Enforced by
  `max_turns_stops_a_loop_that_never_ends` (`:1471`) and
  `max_turns_still_outranks_max_rounds` (`:1990`).
- A round closes on the last straggler, not on a repeat (`record_turn`,
  `crates/kerness/src/orchestrator.rs:279`). Enforced by
  `it_takes_the_last_straggler_and_not_a_repeat_to_close_one` (`:1748`),
  which also drives the host-driven interface through the same counters.
- `parse_result_fields` returns one entry per declared field, always
  (`crates/kerness/src/orchestrator.rs:992`): a model that ignores the instruction yields type-appropriate
  defaults rather than an error, because failing the closing turn would
  discard the whole transcript over a formatting mistake. Enforced by
  `the_declared_fields_alone_decide_what_comes_back` (`:1576`) and
  `every_declared_type_and_alias_is_coerced` (`:1608`).
- Restoration rejects inconsistent progress rather than repairing it
  (`crates/kerness/src/orchestrator.rs:743`, `:751`). Enforced by
  `a_saved_index_past_the_end_of_an_edited_gameplan_is_clamped` (`:2218`) and
  the corrupt-counter cases in `crates/kerness/tests/resume.rs`.
- Delivery is the checkpoint boundary: the position published to
  `record_position` at each delivery already reflects that turn. Enforced by
  `a_resumed_run_carries_its_predecessors_turn_count_and_phase`
  (`crates/kerness/src/orchestrator.rs:2112`).
- No test enforces that `OrchestratorLoop` performs no IO; it holds no
  provider, channel, or path, which is the structural guarantee.

## Key Types and Entry Points

- `crates/kerness/src/orchestrator.rs:49` — `LoopHost` — the blocking
  adapter's callbacks: two provider turns, delivery, note, directive, the
  closing turn, the summary record, and an optional `record_position`.
- `crates/kerness/src/orchestrator.rs:91` — `EndReason`; `:121` `LoopState` —
  turn and round counts, consensus, phase reached, summary, fields, and why
  the loop stopped; `as_str` (`:108`) is the spelling `SessionResult`
  reports.
- `crates/kerness/src/orchestrator.rs:379` — `LoopTurnKind`; `:388`
  `LoopAction` — a requested turn, delivery, directive, note, summary, or
  completed state. Reading one does no IO and does not consume it.
- `crates/kerness/src/orchestrator.rs:436` — `OrchestratorLoop`; `new`
  (`:459`) takes the `LoopSpec` and the roster; `with_result_fields`
  (`:483`), `with_max_turns` (`:489`), `with_retries` (`:495`) and
  `with_resume_state` (`:501`) are the builders.
- `crates/kerness/src/orchestrator.rs:567` — `next_action` — validates and
  initializes on first call; `Error::Session` for a bad snapshot.
- `crates/kerness/src/orchestrator.rs:613` — `submit_reply`; `:649`
  `acknowledge` — consume a requested turn or a queued callback; each is an
  error when nothing of that kind is pending.
- `crates/kerness/src/orchestrator.rs:711` — `snapshot` — version-1 counters
  plus the `scheduler` object; the resume map verbatim before initialization.
- `crates/kerness/src/orchestrator.rs:510` — `run` — drives the same actions
  through a `LoopHost` to a `LoopState`.
- `crates/kerness/src/orchestrator.rs:679` — `commit_host_turn`; `:700`
  `host_limit_reached` — the host-driven interface; `raw_closing_result`
  (`:664`) preserves the final uncoerced reply for strict validation.
- `crates/kerness/src/orchestrator.rs:935` — `closing_prompt`; `:965`
  `verdict_rethink_prompt`; `:992` `parse_result_fields`; `:1011`
  `strip_result_block` — the closing turn's text, in and out.

## Interactions

- [run.md](run.md) holds the loop as its `scheduler`
  (`crates/kerness/src/session/run.rs:266`), steps it through `next_action`,
  `submit_reply`, `acknowledge`, and the host-driven pair, and reads
  `raw_closing_result` for strict validation (`:1110`). The shared state is
  `LoopAction`; integration is proved in `crates/kerness/tests/session_run.rs`.
- [session.md](session.md) constructs the loop at preparation from the
  harness's `LoopSpec`, result fields, and any saved position
  (`crates/kerness/src/session.rs:1116`), and implements `LoopHost` for the
  compatibility adapter (`:1846`).
- [agent-runtime.md](agent-runtime.md) turns each requested `Turn` into
  provider and tool steps without the loop knowing how many.
- [harness.md](harness.md) supplies `LoopSpec`, `PhaseSpec`, and
  `ResultField`; the loop reads `terminate_on`, `advance_on`, `max_turns`,
  `max_rounds`, `orchestrator_retries`, `verdict_rethink`, and the phases.
- [sessionfile.md](sessionfile.md) stores `snapshot()` under the snapshot's
  `loop` key (`crates/kerness/src/sessionfile.rs:54`) beside the conversation
  and any active agent continuation.
- [utils.md](utils.md) supplies `parse_session_end`, `parse_orchestrator_call`
  and `keyword_in_text` (`crates/kerness/src/utils.rs:64`, `:79`, `:34`), the
  three scans every orchestrator reply goes through.
- The Python `OrchestratorLoop` (`bindings/python/src/runtime.rs:649`) exposes
  `run` and `snapshot` only; stepping from Python goes through
  `SessionRun` ([bindings.md](bindings.md)).

## How to Test

```sh
cargo test -p kerness --lib orchestrator                                                  # pass = 50 passed
.venv/bin/python -m pytest bindings/python/tests/test_loop.py -q                          # pass = 40 passed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q -k TestResumingFromASessionFile # pass = 6 passed
```

- Termination and routing: `the_declared_keyword_ends_it_and_an_undeclared_one_does_not`
  (`crates/kerness/src/orchestrator.rs:1315`),
  `retries_exhaust_into_a_forced_end_and_zero_means_none` (`:1388`),
  `max_turns_stops_a_loop_that_never_ends` (`:1471`); from Python,
  `TestTerminationComesFromTheHarness`, `TestRouting`, `TestRetries`, and
  `TestLimits` (`bindings/python/tests/test_loop.py:100`, `:141`, `:156`,
  `:197`).
- Phases: `the_phase_rides_every_routed_turn_without_displacing_the_ask`
  (`crates/kerness/src/orchestrator.rs:1682`), `it_takes_the_last_straggler_and_not_a_repeat_to_close_one`
  (`:1748`, which also drives `commit_host_turn` and `host_limit_reached`
  directly, including unknown-agent refusal and the bound),
  `the_briefing_names_who_still_owes_a_turn_and_is_reissued` (`:1787`),
  `the_last_phase_running_out_stops_the_loop_and_still_closes` (`:1845`),
  `the_keyword_advances_the_phase_whether_or_not_it_routes` (`:1877`),
  `max_rounds_caps_a_single_phase` (`:1938`); from Python,
  `TestPhasesReachParticipants` through `TestMaxRoundsIsRealNow`
  (`bindings/python/tests/test_loop.py:386` onward).
- Closing: `declared_fields_are_named_in_the_prompt_and_nothing_else_is`
  (`crates/kerness/src/orchestrator.rs:1523`), `the_object_is_read_fenced_or_bare` (`:1561`),
  `every_declared_type_and_alias_is_coerced` (`:1608`),
  `the_draft_is_revised_and_only_the_revision_is_kept` (`:2024`),
  `the_verdict_rethink_is_on_by_default_and_a_harness_can_turn_it_off`
  (`:2081`); from Python, `TestTheJudgeRethinksItsVerdict`
  (`bindings/python/tests/test_loop.py:620`).
- Resume: `a_resumed_run_carries_its_predecessors_turn_count_and_phase`
  (`crates/kerness/src/orchestrator.rs:2112`) — delivery is the checkpoint boundary — and
  `a_saved_index_past_the_end_of_an_edited_gameplan_is_clamped` (`:2218`);
  the session-level owner `TestResumingFromASessionFile`
  (`bindings/python/tests/test_session.py:2923`) covers the legacy completed
  run extension, the phase position in the file (`:2959`), and an interrupted
  run resuming at its exact pending action (`:3018`); corrupt counters and a
  restore/save cycle before the first action are in
  `crates/kerness/tests/resume.rs`.
- Through a session: `a_terminator_ends_the_run_and_says_so`
  (`crates/kerness/tests/session_run.rs:72`),
  `the_declared_result_fields_come_back_typed` (`:134`),
  `the_phase_list_running_out_ends_the_session` (`:816`),
  `zero_rounds_goes_straight_to_the_closing_turn` (`:867`),
  `the_turn_budget_stops_the_loop` (`:909`),
  `an_unroutable_orchestrator_is_retried_and_then_forced` (`:945`), and
  `a_host_selects_an_agent_and_finishes_without_a_judge_call` (`:257`).
- Gap: `host_briefing` (`crates/kerness/src/orchestrator.rs:673`) has no direct test; it is reached only through
  the owned run.

## Review and Refactor Guide

- **Adding a `LoopAction` variant** → `next_action`
  (`crates/kerness/src/orchestrator.rs:567`), the blocking `run` match
  (`:510`), `SessionRun::apply_effect`
  (`crates/kerness/src/session/run.rs:607`), the restore check that refuses
  turn-like actions in the queue (`crates/kerness/src/orchestrator.rs:760`),
  and a `StubHost` case. The variant
  name is a checkpoint tag.
- **Changing when a round or phase closes** → `PhaseTracker::record_turn`
  (`crates/kerness/src/orchestrator.rs:279`), `next_phase` (`:359`),
  `structure_complete` (`:834`), and the phase tests from `:1682`;
  [harness.md](harness.md)'s `max_rounds` clamp (`:179`) applies per phase.
- **Changing the closing prompts or parser** → `closing_prompt`
  (`crates/kerness/src/orchestrator.rs:935`),
  `verdict_rethink_prompt` (`:965`), `parse_result_fields` (`:992`) and
  `coerce` (`:1048`); the strict path in [run.md](run.md) reads
  `raw_closing_result` instead and must not be handed coerced text. Python
  tests match on prompt wording (`bindings/python/tests/test_loop.py:227`,
  `:686`).
- **Changing the snapshot shape** → `SavedScheduler`
  (`crates/kerness/src/orchestrator.rs:427`),
  `PhaseTracker::snapshot`/`restore` (`:301`, `:322`), `initialize_mode`
  (`:738`), the version-1 fallback, and both resume owners
  (`crates/kerness/tests/resume.rs`,
  `bindings/python/tests/test_session.py:2923`). A version-1 file must keep
  loading.
- **Changing an orchestrator hint or briefing** → `hint`
  (`crates/kerness/src/orchestrator.rs:915`),
  `standing_briefing` (`:813`), `PhaseTracker::briefing` (`:234`); the
  bundled `orchestrator.md` role ([role.md](role.md)) describes the same
  protocol to the model.
- Safe extension points: `LoopHost` default methods (only `record_position`
  is defaulted today); the builders on `OrchestratorLoop`.
- Forbidden coupling: no provider, channel, conversation, or filesystem
  handle in this module; a loop that reaches a resource cannot be stepped by
  a host.
- Compatibility: `EndReason::as_str` spellings and the `LoopState` fields
  reach callers through `SessionResult`; the Python `OrchestratorLoop`
  keyword signature is public.

Improvement candidates (proposals, not accepted work):

- A `LoopHost` trait-object test for `host_briefing` would close the one
  direct gap named above. Success check: the briefing text is asserted
  outside a `SessionRun`.
- `parse_result_fields` and `session/outcome.rs` read the same closing reply
  twice with different rules; a single extraction shared by both would remove
  the double `extract_fenced_json` pass, once the legacy coercing path is
  retired.

## Open Gaps / Roadmap

- The orchestrator addresses one participant at a time; host-driven sessions
  can choose another schedule but tool/provider execution remains
  synchronous.
- Phase transitions are forward-only.
- `parse_result_fields` remains the legacy coercing parser. Strict validation
  belongs to the session outcome layer and uses `raw_closing_result`.
