---
eatmycode_version: "1.2.0"
---

# Compaction

## Goal

A long session outgrows the model's context window. Compaction is the answer:
estimate how large the turn history is, and when it crosses the ceiling, replace
older turns with a summary, preserving the topic and a recent suffix verbatim.
The suffix targets half the token allowance, not half the number of turns
(`crates/kerness/src/compaction.rs:104`).

The estimate is deliberately crude — characters divided by four — because the
alternative is a tokenizer per model family, and the ceiling exists to stay
under a hard limit, not to fill it exactly. Being crude means being wrong
sometimes, which is why there are two passes: a check before each new turn,
and a retry after a provider says the check was wrong.

This module owns the estimate, the rewrite, and the summary request. It does
not own the ceiling, the summarizer, or where the result is stored: the session
subtracts the rest of the request from the window and hands over the remainder,
and the run engine decides when a pass happens and who summarizes.

## Status

`done` — `cargo test -p kerness compaction` passes 8 tests (7 unit, 1
integration), `cargo test -p kerness --test compaction_e2e` passes all 8
integration tests, and `bindings/python/tests/test_compaction.py` passes 9.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/compaction.rs` | estimation, the rewrite, and the summary request |
| `crates/kerness/src/session.rs:1752` | `fit_conversation`, the ceiling and the overhead it leaves |
| `crates/kerness/src/session.rs:1806` | `summarize`, the provider call the closure wraps |
| `crates/kerness/src/session/run.rs:724` | `advance_turn`, separate compaction and provider steps, with one overflow retry |
| `bindings/python/src/funcs.rs:229` | the four functions and, in `register`, the four constants |
| `bindings/python/kerness/compaction.py` | re-export shim |
| `crates/kerness/tests/compaction_e2e.rs` | the passes driven through a whole session |

## Language and Conventions

Rust crate module, with a PyO3 binding in `funcs.rs` and a Python re-export
shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `compaction.rs` depends on `conversation.rs` and nothing else in the crate;
  it has no IO and returns plain values, so nothing here returns `Result`.
- The summarizer crosses as a closure, `FnOnce(&[Turn]) -> String`
  (`crates/kerness/src/compaction.rs:86`). The Python binding parks a raising
  summarizer's exception in a `RefCell` and re-raises it after `compact`
  unwinds (`bindings/python/src/funcs.rs:250`), which is the same parking
  pattern [channel.md](channel.md) uses; it is observed here, enforced by no
  test.
- The unit tests build turns with a local `said(speaker, content)` helper
  (`crates/kerness/src/compaction.rs:163`); the integration file drives a real
  gameplan against `ScriptedProvider` from `crates/kerness/tests/common/mod.rs`,
  keyed on the `compaction` purpose. The Python tests build conversations with
  `_conversation(n, size)` (`bindings/python/tests/test_compaction.py:25`) and
  fail on any summarizer call that should not happen through `_unused` (`:150`).

## Design and Invariants

`compact` takes the summarizer as a closure rather than a provider, so the
rewrite is testable without a network call and a caller can summarize with a
cheaper model than the one running the session. The module never sees the
ceiling: `fit_conversation` computes what the conversation may use and passes
only that.

### The ceiling is per agent, and the conversation gets what is left

`fit_conversation` (`crates/kerness/src/session.rs:1752`) runs before each
turn's first provider operation. In an owned run, compaction is a separate step
so a single step cannot buy both a summary and an agent response
(`crates/kerness/src/session/run.rs:733`). It works out two figures:

- The **ceiling** (`context_ceiling`, `session.rs:1738`) is the smaller of
  `max_context_tokens` — what the caller is willing to spend — and the
  provider's own window for that agent's model
  ([`Provider::context_window`](provider.md)). A mixed-provider session has one
  figure per model, and compacting the whole run against the largest of them
  would fail on every turn taken by the smallest.
- The **overhead** (`prompt_overhead`, `session.rs:1706`) is the assembled
  system message plus, under a native dialect, the tool schemas that travel in
  the request body. Under text the schemas are already inside the system message,
  so counting them again would charge the caller twice.

The conversation may use the difference. That is why memory, the persona, the
skill index, the context blocks, and the permitted tool set all narrow what
history survives, and why the measurement is per turn rather than once: those
differ by agent, and memory grows during the run. An overhead that meets or
exceeds the ceiling is a named session error (`session.rs:1755`) rather than
something to hand to the provider — compaction cannot touch the system prompt,
so no amount of summarizing would make it fit, and continuing would buy a
summary call per turn and still fail.

### The reactive pass

The estimate can be wrong in the direction that matters, and the provider is the
authority. `advance_turn` (`crates/kerness/src/session/run.rs:817`) catches a
refusal that [errors.md](errors.md)'s `is_context_overflow` recognises, marks a
single overflow retry, and schedules compaction to `OVERFLOW_RETRY_FRACTION` of
the allowance before the next provider step. The retained turn state preserves
tool exchanges already completed before the refusal.

The fraction is not `1.0` for a concrete reason: re-measuring against the same
allowance would find the conversation already fits and change nothing, so the
retry would be the same request refused twice. Half is the same step
`COMPACT_TO_FRACTION` takes, for the same reason — big enough that one retry is
likely to be the only one, small enough not to throw the conversation away over
a heuristic that was slightly off.

Once, not in a loop. A second refusal means the shortfall is not in the
conversation, and going round again would buy a summary call per attempt and be
refused each time.

### Invariants

- **The topic and the newest turn survive.** The first turn anchors the
  conversation (`crates/kerness/src/compaction.rs:102`) and the most recent
  turn is kept unconditionally (`:114`). Enforced by
  `the_topic_and_the_latest_turn_always_survive` (`:180`) and
  `the_topic_survives_compaction` (`crates/kerness/tests/compaction_e2e.rs:178`).
- **A failed summary changes nothing.** An empty or whitespace summary returns
  `None` (`crates/kerness/src/compaction.rs:128`), and `summarize` answers a
  provider failure with an empty string after logging a warning
  (`crates/kerness/src/session.rs:1834`), so real turns are never traded for
  nothing. Enforced by `an_empty_summary_leaves_the_conversation_intact`
  (`crates/kerness/src/compaction.rs:200`) and
  `an_empty_summary_leaves_the_history_alone`
  (`crates/kerness/tests/compaction_e2e.rs:193`). A non-provider error from
  the summarizer propagates instead (`session.rs:1837`).
- **The transcript is never compacted.** `replace_turns` rewrites the turns
  and leaves the transcript alone ([conversation.md](conversation.md)); the
  transcript is never sent to a model, so shrinking it would cost the caller
  their report. Enforced by
  `test_a_long_run_compacts_the_prompt_and_only_the_prompt`
  (`bindings/python/tests/test_session.py:3075`).
- **`None` means untouched, not a copy.** Callers use the distinction to decide
  whether to save; `compact` returns `None` when the history fits, when there
  is one turn, when nothing would be dropped, and when the summary is empty
  (`crates/kerness/src/compaction.rs:90`, `:93`, `:122`, `:128`).
- **The estimate counts rendered characters.** `estimate_turns` measures
  `Turn::render` output, so the `[Speaker] ` prefix is charged, and characters
  rather than bytes, so non-ASCII prose is not charged twice. Enforced by
  `estimates_count_the_rendered_form` (`crates/kerness/src/compaction.rs:213`)
  and `the_estimate_counts_characters_rather_than_bytes`
  (`crates/kerness/tests/compaction_e2e.rs:294`).
- **A compaction is metered and recorded.** The run engine wraps
  `fit_conversation` in a usage scope under the purpose `compaction`
  (`crates/kerness/src/session/run.rs:743`), the session counts it in
  `compactions` (`crates/kerness/src/session.rs:1787`) and the count is saved
  in the session file. Enforced by
  `the_session_file_records_how_often_it_compacted`
  (`crates/kerness/tests/compaction_e2e.rs:220`) and
  `collector_attributes_compaction_closing_and_opaque_overrides`
  (`crates/kerness/src/usage.rs:751`).

## Key Types and Entry Points

- `crates/kerness/src/compaction.rs:33` — `CHARS_PER_TOKEN` — the estimate's only
  parameter; `4`.
- `crates/kerness/src/compaction.rs:40` — `COMPACT_TO_FRACTION` — how much of the
  limit the compacted history targets: half.
- `crates/kerness/src/compaction.rs:46` — `SUMMARY_PREFIX` / `:49`
  `SUMMARY_PROMPT` — the marker the replacement turn opens with, and what the
  summarizing model is asked to produce.
- `crates/kerness/src/compaction.rs:62` — `estimate_tokens(text)` / `:70`
  `estimate_turns(turns)` — the estimate over character count of the rendered
  form, with no intermediate render allocated for `estimate_tokens`.
- `crates/kerness/src/compaction.rs:86` — `compact(turns, limit, summarize)` —
  returns `Some(rewritten)` or `None` to leave the history alone; calls
  `summarize` at most once, with the dropped turns only.
- `crates/kerness/src/compaction.rs:147` — `summary_request(turns)` — the
  system-plus-user message pair sent to the summarizing model; no tools, no
  persona.
- `crates/kerness/src/session.rs:77` — `OVERFLOW_RETRY_FRACTION` — how far the
  conversation is compacted on the retry; `0.5`.
- `crates/kerness/src/session.rs:1752` — `fit_conversation(agent, base_prompt,
  fraction)` — computes ceiling and overhead, calls `compact`, replaces the
  turns, emits a system note, and saves; a prompt that alone exceeds the
  ceiling is `Error::Session` with both numbers.
- `crates/kerness/src/session.rs:1806` — `summarize(turns)` — one metered
  provider call through the orchestrator's (or first agent's) provider; a
  provider error becomes an empty string, any other error propagates.
- `bindings/python/src/funcs.rs:250` — `compact(turns, *, limit, summarize)` —
  the pyfunction; a raising Python summarizer is re-raised as the caller's
  error, not read as an empty summary.

## Interactions

- Called by [run.md](run.md)'s `advance_turn` when preparing a turn or retrying
  overflow, against `DEFAULT_MAX_CONTEXT_TOKENS`
  (`crates/kerness/src/session.rs:66`) or the configured ceiling, whichever the
  agent's provider window does not undercut. The purpose string `compaction`
  is the contract the usage ledger and the integration tests key on.
- Rewrites the turn list owned by [conversation.md](conversation.md) via
  `replace_turns`; the transcript is untouched.
- Its summary comes from a [provider.md](provider.md) call the session makes
  through `chat_with_retries`, so a provider subclass's override is honoured;
  [run.md](run.md) records it under the compaction purpose in the usage
  ledger.
- Measures what [prompting.md](prompting.md) assembled, so every block that
  module renders is overhead here, and [toolschema.md](toolschema.md)'s
  `tool_schemas` decides whether schemas count separately.
- Recognises the overflow refusal through [errors.md](errors.md)'s
  `is_context_overflow`.
- The count of compactions travels in [sessionfile.md](sessionfile.md)'s
  snapshot as `compactions`.
- The four constants and four functions cross to Python through
  [bindings.md](bindings.md); `test_compaction.py` and `test_session.py` prove
  the boundary.

## How to Test

```sh
cargo test -p kerness compaction                                       # pass = 8 passed, 0 failed
cargo test -p kerness --test compaction_e2e                            # pass = 8 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_compaction.py -q # pass = 9 passed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q    # pass = 122 passed
```

- `crates/kerness/src/compaction.rs:168` — `a_conversation_that_fits_is_left_alone`,
  `:174` `a_single_oversized_turn_is_left_alone`, `:180`
  `the_topic_and_the_latest_turn_always_survive`, `:200`
  `an_empty_summary_leaves_the_conversation_intact`, `:213`
  `estimates_count_the_rendered_form`, `:219`
  `the_summary_request_carries_the_prompt_and_the_dropped_prose` — the
  rewrite's contract on its own.
- `crates/kerness/tests/compaction_e2e.rs:131` —
  `a_conversation_over_the_ceiling_is_compacted` (both the blocking and the
  stepped driver), `:160`
  `the_summary_replaces_the_dropped_turns_under_its_own_label`, `:234`
  `a_ceiling_the_prompt_alone_exceeds_is_refused_with_the_numbers`, `:267`
  `an_ordinary_run_under_the_default_ceiling_never_compacts` — the proactive
  pass through a real session.
- `bindings/python/tests/test_compaction.py:47` — `test_it_leaves_a_short_conversation_alone`;
  `:62` `test_the_result_is_topic_summary_then_recent_turns`; `:114`
  `test_the_summarizer_sees_only_the_dropped_turns`; `:98`
  `test_the_result_leaves_room_for_turns_to_come` — the anti-thrash property
  behind `COMPACT_TO_FRACTION`.
- `:85` `test_the_newest_turn_is_kept_even_when_it_alone_is_too_big` and `:53`
  `test_a_single_oversized_turn_is_not_compactable` are the two edge cases that
  decide what `compact` does when halving cannot help.
- `bindings/python/tests/test_session.py:3048` —
  `TestContextLimitKeepsTheConversationSendable` — the proactive pass through a
  live run, including that it compacts the prompt and only the prompt (`:3075`)
  and that everything else in the prompt counts against the limit (`:3110`).
- `bindings/python/tests/test_session.py:3145` —
  `TestAProviderRefusingALongRequestIsRetriedOnce` — the reactive pass: a
  provider that answers 400 with an overflow body once, and a run that reaches
  its summary rather than ending on the refusal (`:3190`).
- Gap: no test drives a summarizer that raises from Python through `compact`
  to prove the parked exception is the caller's; the crate's `summarize` maps
  a provider error to an empty string, which the integration tests cover, but
  the Python parking at `bindings/python/src/funcs.rs:256` is untested.

## Review and Refactor Guide

- **Changing the estimate** (`CHARS_PER_TOKEN`, `estimate_tokens`,
  `estimate_turns`) → inspect `prompt_overhead` and `context_ceiling` in
  `crates/kerness/src/session.rs`, which measure the rest of the request with
  the same function; re-run `compaction`, `compaction_e2e`, and
  `test_session.py -k ContextLimit`. `CHARS_PER_TOKEN` is asserted by
  `crates/kerness/tests/public_api.rs` and `bindings/python/tests/test_provider.py`
  as a well-known constant.
- **Changing what survives** (`compact`'s anchor and keep-from loop) →
  `the_topic_and_the_latest_turn_always_survive`,
  `test_the_result_is_topic_summary_then_recent_turns`, and
  `test_the_result_leaves_room_for_turns_to_come` own the three guarantees;
  `SUMMARY_PREFIX` is what transcripts and the e2e label test match on.
- **Changing when a pass happens** → `advance_turn`'s `needs_fit` and
  `overflow_retry` flags in `crates/kerness/src/session/run.rs` are checkpoint
  state; a new flag must be serialized and validated with the rest of the
  active turn ([sessionfile.md](sessionfile.md)), and
  `TestAProviderRefusingALongRequestIsRetriedOnce` must still see exactly one
  retry.
- **Changing the summarizer call** → `summarize` must keep the `compaction`
  purpose and the `observe_provider_call` wrapper, or the usage ledger and
  `collector_attributes_compaction_closing_and_opaque_overrides` lose the
  record; it must keep answering a provider error with an empty string.
- **Safe extension points**: a different summarizer is a different closure; a
  per-model estimate is a new `estimate_*` function the session calls, not a
  change to `compact`'s signature.
- **Forbidden coupling**: `compaction.rs` must not import the session, the
  provider, or the ceiling; it takes the allowance and a closure. The
  transcript must never be passed to `compact`.
- **Compatibility**: the Python signature `compact(turns, *, limit, summarize)`
  and the four constants are public; `compactions` is a snapshot field.

Improvement candidates, as proposals:

- Bound the summary's length before accepting it, so a verbose summarizer
  cannot return a turn larger than the ones it replaced. Success check: a test
  in which the summary exceeds the dropped turns' estimate is rejected as
  `None`.
- Test the Python parking path at `bindings/python/src/funcs.rs:256` with a
  summarizer that raises. Success check: `test_compaction.py` gains a case
  asserting the caller's exception type propagates.

## Open Gaps / Roadmap

- `estimate_turns` measures the turns alone. The rest of the prompt is counted
  against the ceiling by the session, not by this module, so a caller using
  `compact` directly has to account for it themselves.
- One compaction pass per check: a history far over the limit is halved once, not
  repeatedly, and is compacted again on the next check.
- The summary is not itself bounded; a verbose summarizer can produce a turn
  larger than the ones it replaced.
- `CHARS_PER_TOKEN` is one number for every model and every script. It is
  roughly right for English prose and wrong for CJK text and for dense JSON in a
  tool result, in opposite directions. The reactive pass is what absorbs being
  wrong; a per-model figure would need a tokenizer the framework does not carry.
