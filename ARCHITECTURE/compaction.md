# Compaction

## Goal

A long session outgrows the model's context window. Compaction is the answer:
estimate how large the turn history is, and when it crosses the ceiling, replace
the oldest half with a single summary turn and keep the rest verbatim.

The estimate is deliberately crude — characters divided by four — because the
alternative is a tokenizer per model family, and the ceiling exists to stay
under a hard limit, not to fill it exactly. Being crude means being wrong
sometimes, which is why there are two passes: a check before each new turn,
and a retry after a provider says the check was wrong.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/compaction.rs` | estimation, the rewrite, and the summary request |
| `crates/kerness/src/session.rs:1752` | `fit_conversation`, the ceiling and the overhead it leaves |
| `crates/kerness/src/session/run.rs:724` | `advance_turn`, separate compaction and provider steps, with one overflow retry |
| `bindings/python/src/funcs.rs` | the four functions and four constants, re-exported |
| `bindings/python/kerness/compaction.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/compaction.rs:33` — `CHARS_PER_TOKEN` — the estimate's only
  parameter.
- `crates/kerness/src/compaction.rs:40` — `COMPACT_TO_FRACTION` — how much of the
  history is summarized away: half.
- `crates/kerness/src/compaction.rs:46` — `SUMMARY_PREFIX` — the marker the
  replacement turn opens with, so a reader of the transcript can see where the
  seam is.
- `crates/kerness/src/compaction.rs:49` — `SUMMARY_PROMPT` — what the model is
  asked to produce.
- `crates/kerness/src/compaction.rs:62` — `estimate_tokens(text)` / `:70`
  `estimate_turns(turns)` — the estimate over character count, with no
  intermediate render allocated.
- `crates/kerness/src/compaction.rs:86` — `compact(turns, limit, summarize)` —
  returns `None` when the history is under the limit, so the caller can tell
  "nothing to do" from "rewritten".
- `crates/kerness/src/compaction.rs:147` — `summary_request(turns)` — the messages
  sent to the summarizing model.
- `crates/kerness/src/session.rs:77` — `OVERFLOW_RETRY_FRACTION` — how far the
  conversation is compacted on the retry.

`compact` takes the summarizer as a closure rather than a provider, so the
rewrite is testable without a network call and a caller can summarize with a
cheaper model than the one running the session.

### The ceiling is per agent, and the conversation gets what is left

`fit_conversation` (`session.rs:1752`) checks each new turn before its first
provider operation. In an owned run, compaction is a separate step so a single
step cannot buy both a summary and an agent response. It works out two figures:

- The **ceiling** (`context_ceiling`, `session.rs:1736`) is the smaller of
  `max_context_tokens` — what the caller is willing to spend — and the
  provider's own window for that agent's model
  ([`Provider::context_window`](provider.md)). A mixed-provider session has one
  figure per model, and compacting the whole run against the largest of them
  would fail on every turn taken by the smallest.
- The **overhead** (`prompt_overhead`, `session.rs:1706`) is the assembled
  system message plus, under a native dialect, the tool schemas that travel in
  the request body. Under text the schemas are already inside the system message,
  so counting them again would charge the caller twice.

The conversation may use the difference. That is why memory, the
persona, the skill index, the context blocks, and the permitted tool set all
narrow what history survives, and why the measurement is per turn rather than
once: those differ by agent, and memory grows during the run. An
overhead that meets or exceeds the ceiling is a named session error
(`session.rs:1755`) rather than something to hand to the provider — compaction
cannot touch the system prompt, so no amount of summarizing would make it fit,
and continuing would buy a summary call per turn and still fail.

### The reactive pass

The estimate can be wrong in the direction that matters, and the provider is the
authority. `SessionRun::advance_turn` (`session/run.rs:724`) catches a refusal
that [errors.md](errors.md)'s `is_context_overflow` recognises, marks a single
overflow retry, and schedules compaction to `OVERFLOW_RETRY_FRACTION` of the
allowance before the next provider step. The retained turn state preserves tool
exchanges already completed before the refusal.

The fraction is not `1.0` for a concrete reason: re-measuring against the same
allowance would find the conversation already fits and change nothing, so the
retry would be the same request refused twice. Half is the same step
`COMPACT_TO_FRACTION` takes, for the same reason — big enough that one retry is
likely to be the only one, small enough not to throw the conversation away over
a heuristic that was slightly off.

Once, not in a loop. A second refusal means the shortfall is not in the
conversation, and going round again would buy a summary call per attempt and be
refused each time.

## Interactions

- Called by [run.md](run.md) when preparing a turn or retrying overflow, against
  `DEFAULT_MAX_CONTEXT_TOKENS` (`session.rs:66`) or the configured ceiling,
  whichever the agent's provider window does not undercut.
- Rewrites the turn list owned by [conversation.md](conversation.md) via
  `replace_turns`.
- Its summary comes from a [provider.md](provider.md) call the session makes;
  [run.md](run.md) records it under the compaction purpose in the usage ledger.
- Measures what [prompting.md](prompting.md) assembled, so every block that
  module renders is overhead here.

## How to Test

```sh
cargo test -p kerness compaction                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_compaction.py -q # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q    # pass = 0 failed
```

- `bindings/python/tests/test_compaction.py:47` — `test_it_leaves_a_short_conversation_alone`;
  `:62` `test_the_result_is_topic_summary_then_recent_turns`; `:114`
  `test_the_summarizer_sees_only_the_dropped_turns`.
- `:85` `test_the_newest_turn_is_kept_even_when_it_alone_is_too_big` and `:53`
  `test_a_single_oversized_turn_is_not_compactable` are the two edge cases that
  decide what `compact` does when halving cannot help.
- `bindings/python/tests/test_session.py:3048` —
  `TestContextLimitKeepsTheConversationSendable` — the proactive pass through a
  live run, including that it compacts the prompt and only the prompt: the
  transcript is never sent to a model, so shrinking it would silently cost the
  caller their report.
- `bindings/python/tests/test_session.py:3145` —
  `TestAProviderRefusingALongRequestIsRetriedOnce` — the reactive pass: a
  provider that answers 400 with an overflow body once, and a run that reaches
  its summary rather than ending on the refusal.

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
