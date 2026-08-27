# Compaction

## Goal

A long session outgrows the model's context window. Compaction is the answer:
estimate how large the turn history is, and when it crosses the ceiling, replace
the oldest half with a single summary turn and keep the rest verbatim. Serves
**M1**.

The estimate is deliberately crude — characters divided by four — because the
alternative is a tokenizer per model family, and the ceiling exists to stay
under a hard limit, not to fill it exactly.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/compaction.rs` | estimation, the rewrite, and the summary request |
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
  `estimate_turns(turns)` — the estimate over byte length, with no intermediate
  render allocated.
- `crates/kerness/src/compaction.rs:86` — `compact(turns, limit, summarize)` —
  returns `None` when the history is under the limit, so the caller can tell
  "nothing to do" from "rewritten".
- `crates/kerness/src/compaction.rs:143` — `summary_request(turns)` — the messages
  sent to the summarizing model.

`compact` takes the summarizer as a closure rather than a provider, so the
rewrite is testable without a network call and a caller can summarize with a
cheaper model than the one running the session.

## Interactions

- Called by [session.md](session.md) before each provider call, against
  `DEFAULT_MAX_CONTEXT_TOKENS` (`session.rs:54`) or the configured ceiling.
- Rewrites the turn list owned by [conversation.md](conversation.md) via
  `replace_turns`.
- Its summary comes from a [provider.md](provider.md) call the session makes.

## How to Test

```sh
cargo test -p kerness compaction                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_compaction.py -q # pass = 0 failed
```

- `bindings/python/tests/test_compaction.py:49` — `test_it_leaves_a_short_conversation_alone`;
  `:64` `test_the_result_is_topic_summary_then_recent_turns`; `:116`
  `test_the_summarizer_sees_only_the_dropped_turns`.
- `:87` `test_the_newest_turn_is_kept_even_when_it_alone_is_too_big` and `:55`
  `test_a_single_oversized_turn_is_not_compactable` are the two edge cases that
  decide what `compact` does when halving cannot help.

## Open Gaps / Roadmap

- `estimate_turns` measures the turns alone. The rest of the prompt — system
  prompt, tool schemas, memory block — is counted against the ceiling by the
  session, not by this module (`bindings/python/tests/test_session.py:2209`), so a caller using
  `compact` directly has to account for it themselves.
- One compaction pass per check: a history far over the limit is halved once, not
  repeatedly, and is compacted again on the next check.
- The summary is not itself bounded; a verbose summarizer can produce a turn
  larger than the ones it replaced.
