---
eatmycode_version: "1.1.0"
---

# Conversation

## Goal

The session's memory of what has been said, in two shapes at once. `turns` is
the structured record — who spoke, in which round, of what kind — and
`transcript` is the flat message list a harness prints or saves. `render()`
turns the first into the `ChatMessage` list a provider is actually sent.

Keeping both is what lets compaction rewrite the history without losing the
transcript, and lets a resumed session restore each independently. This module
owns the three record types and the two lists; it does not decide what gets
said, when a turn is recorded, or how a saved run is validated.

## Status

`done` — `cargo test -p kerness conversation` passes 8 tests and
`bindings/python/tests/test_conversation.py` passes 4.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/conversation.rs` | `ChatMessage`, `Message`, `Turn`, `Conversation` |
| `bindings/python/src/runtime.rs:75` | `PyConversation` |
| `bindings/python/src/types.rs` | `PyMessage` (`bindings/python/src/types.rs:487`), `PyTurn` (`:566`) |
| `bindings/python/src/funcs.rs:667` | `render_turn`, one turn to its chat dict |
| `bindings/python/src/convert.rs:113` | `chat_message_to_py`, the shared `{role, content}` dict shape |
| `bindings/python/kerness/conversation.py` | re-export shim |

## Language and Conventions

Rust crate module, with pyclasses in the runtime and types binding modules and a
Python re-export shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `conversation.rs` imports nothing else from the crate. Nothing here does IO,
  so nothing returns `Result`.
- `ChatMessage`, `Message`, and `Turn` derive `Serialize`/`Deserialize`
  (`crates/kerness/src/conversation.rs:15`, `:35`, `:57`) and are the one
  field definition each record has; [sessionfile.md](sessionfile.md) reuses
  them after envelope validation rather than redeclaring the shape. They are
  not `deny_unknown_fields`, unlike the run continuation types.
- `PyTurn` is a `frozen` pyclass (`bindings/python/src/types.rs:566`);
  `PyMessage` is not (`:487`). Both implement `__eq__` by comparing the inner
  record (`:624`, `:548`), which is what lets the Python tests write
  `result[0] == turns[0]`.
- Constructor defaults are declared in `#[pyo3(signature)]`: `round_idx=0`
  and `msg_type="turn"` on `Turn`, `Message`, and `Conversation.say`
  (`bindings/python/src/types.rs:575`, `:496`;
  `bindings/python/src/runtime.rs:99`).
- Tests are inline unit tests with no fixtures; the Python tests build a
  `Conversation()` directly and compare against literal dicts.

## Design and Invariants

One structured record, rendered on demand. Holding the conversation
provider-shaped — speaker attribution baked into a string — would leave nothing
to render a *different* way, which is exactly what a mixed-provider session
needs. `Turn` therefore keeps the speaker as a field, and `Turn::render`
(`crates/kerness/src/conversation.rs:91`) puts the `[Speaker] ` prefix back on
an assistant turn while passing a directive through unchanged.

Two records are kept because they are not the same thing. Directives the
session injects — the topic, a retry hint, the closing summary request — are
part of what the model reads but are not something an agent *said*, so they
never reach the transcript. System notices are the mirror image: reported to
the caller, never shown to a model. `say` is the only method that writes both.

### Invariants

- **Directives never reach the transcript; notes never reach the model.**
  `directive` and `raw` push a turn only (`crates/kerness/src/conversation.rs:122`,
  `:131`); `note` pushes a transcript entry only (`:153`). Enforced by
  `directives_never_reach_the_transcript_and_notes_never_reach_the_model`
  (`:225`) and
  `test_it_holds_what_was_said_and_noted_but_not_what_was_directed`
  (`bindings/python/tests/test_conversation.py:40`).
- **Compaction rewrites the turns and leaves the transcript whole.**
  `replace_turns` (`crates/kerness/src/conversation.rs:181`) touches `turns` only, because the transcript is
  never sent to a model and shrinking it would silently shorten the caller's
  report. Enforced by `replacing_turns_leaves_the_transcript_whole` (`:242`).
- **The rendered shape is frozen.** An assistant turn renders as
  `{"role": ..., "content": "[Speaker] text"}` and a directive as its content
  alone. Enforced by `an_agent_turn_regains_its_speaker_prefix`
  (`crates/kerness/src/conversation.rs:210`),
  `a_directive_is_rendered_unchanged` (`:219`), and
  `test_agents_are_prefixed_and_directives_are_not`
  (`bindings/python/tests/test_conversation.py:7`); the session suite asserts
  against the same shape.
- **A raw message claims no speaker.** Tool exchanges kept in the shared
  history arrive through `raw` with their `[Tool:name]` prefix as content, so
  they render verbatim and are absent from the transcript. Enforced by
  `test_it_passes_content_through_and_claims_no_speaker`
  (`bindings/python/tests/test_conversation.py:64`).
- **Accessors across the boundary hand back fresh lists.** `render`, `turns`,
  and `transcript` on `PyConversation` build new Python lists each call
  (`bindings/python/src/runtime.rs:110`, `:115`, `:126`), so a caller
  mutating what it got cannot edit the record of a run. Enforced by
  `test_both_accessors_hand_back_a_fresh_list`
  (`bindings/python/tests/test_conversation.py:25`).
- **Restore replaces both lists together.** `restore`
  (`crates/kerness/src/conversation.rs:186`) takes turns and
  transcript as one call because a snapshot holds both; the session calls it
  once at `crates/kerness/src/session.rs:1669`. No test in this module drives
  `restore`; [sessionfile.md](sessionfile.md)'s round-trip test
  `every_kind_of_record_survives` (`crates/kerness/src/sessionfile.rs:367`)
  covers it.

## Key Types and Entry Points

- `crates/kerness/src/conversation.rs:16` — `ChatMessage` — role and content; the
  wire shape a provider takes.
- `crates/kerness/src/conversation.rs:36` — `Message` — sender, content,
  `round_idx`, and `msg_type` (`turn`, `orchestrator`, `summary`, `system`, or
  `final_summary`); the transcript shape a human reads, returned in
  `SessionResult::history`.
- `crates/kerness/src/conversation.rs:58` — `Turn` — a role, a speaker (empty for
  a directive), content without prefix, a round index, and a message type;
  `render()` at `:91` is how it becomes a `ChatMessage`.
- `crates/kerness/src/conversation.rs:122` — `directive(content)` — a user-role
  instruction inserted into the history; not transcribed.
- `crates/kerness/src/conversation.rs:131` — `raw(role, content)` — an
  already-rendered message stored verbatim with no speaker; not transcribed.
- `crates/kerness/src/conversation.rs:136` — `say(speaker, content, round, type)` —
  the normal path: appends to both the turns and the transcript.
- `crates/kerness/src/conversation.rs:153` — `note(content)` — a system notice
  for the caller only, as a `system` transcript entry.
- `crates/kerness/src/conversation.rs:163` — `render()` — the full `ChatMessage`
  list; allocates a fresh vector, which is why the Python side exposes it as a
  method rather than a property.
- `crates/kerness/src/conversation.rs:181` — `replace_turns(turns)` — what
  compaction calls; the transcript is untouched.
- `crates/kerness/src/conversation.rs:186` — `restore(turns, transcript)` — what a
  resumed session calls; the two lists are restored together because a snapshot
  holds both.

## Interactions

- Owned by [session.md](session.md), one per run. The session writes through
  `say` (`crates/kerness/src/session.rs:1630`, `crates/kerness/src/session/run.rs:1056`),
  `directive`, `note`, and — when `tool_results_in_history` is set — `raw`
  (`crates/kerness/src/session/run.rs:1037`).
- Rewritten by [compaction.md](compaction.md) through `replace_turns`; the
  turn list is what `compact` measures and rewrites.
- Rendered into provider calls by [agent-runtime.md](agent-runtime.md): the
  session calls `render()` and hands the `ChatMessage` list to the prompt
  assembler as history.
- Saved and restored by [sessionfile.md](sessionfile.md), which stores
  `turns` and `transcript` as two fields of the snapshot and deserializes
  them through this module's serde impls.
- `Message` is the element type of `SessionResult::history`
  (`crates/kerness/src/session.rs:85`), so its fields are part of the public
  result.
- The `{role, content}` dict shape crosses to Python through
  `chat_message_to_py` ([bindings.md](bindings.md)), shared with summary
  requests and single-turn rendering.

## How to Test

```sh
cargo test -p kerness conversation                                       # pass = 8 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_conversation.py -q # pass = 4 passed
```

- `crates/kerness/src/conversation.rs:210` — `an_agent_turn_regains_its_speaker_prefix`,
  `:219` `a_directive_is_rendered_unchanged`, `:225`
  `directives_never_reach_the_transcript_and_notes_never_reach_the_model`,
  `:242` `replacing_turns_leaves_the_transcript_whole` — the four rules the
  module exists for.
- `bindings/python/tests/test_conversation.py:7` — `test_agents_are_prefixed_and_directives_are_not`
  — the frozen rendered shape, from Python.
- `bindings/python/tests/test_conversation.py:25` — `test_both_accessors_hand_back_a_fresh_list` —
  `render()` and `transcript()` are methods returning fresh lists, so mutating
  what a caller got back cannot corrupt the conversation.
- `bindings/python/tests/test_conversation.py:40` —
  `test_it_holds_what_was_said_and_noted_but_not_what_was_directed` — the two
  records diverge, and `round_idx` and `msg_type` tell the closing summary
  apart from an ordinary turn.
- `bindings/python/tests/test_conversation.py:64` —
  `test_it_passes_content_through_and_claims_no_speaker` — `raw`.
- Gap: `restore` is proved only through the session-file round trip
  (`crates/kerness/src/sessionfile.rs:367`), not here.

## Review and Refactor Guide

- **Adding a field to `Turn` or `Message`** → both are snapshot records
  ([sessionfile.md](sessionfile.md)) and `Message` is in `SessionResult`;
  inspect `PyTurn`/`PyMessage` constructors and getters in
  `bindings/python/src/types.rs`, the `#[pyo3(signature)]` defaults, and
  `every_kind_of_record_survives` in `crates/kerness/src/sessionfile.rs`.
  A new field must have a serde default or the schema version must change.
- **Changing `render`** → the `[Speaker] ` prefix is counted by
  [compaction.md](compaction.md)'s `estimate_turns` and asserted by the
  session suite; run `cargo test -p kerness compaction` and
  `test_session.py` as well as this module's tests.
- **Adding a `msg_type` value** → the values are conventional strings, not an
  enum; search `msg_type` and `"final_summary"` in `crates/kerness/src/session.rs`
  and `session/run.rs` for the readers that match on them.
- **Safe extension points**: a new way to record a message is a new method
  that writes one or both lists explicitly; keep `say` the only method that
  writes both.
- **Forbidden coupling**: `conversation.rs` must not import the session,
  provider, or compaction modules; the dependency runs from them to here.
- **Compatibility**: the Python constructors `Turn(role, speaker, content,
  round_idx=0, msg_type="turn")` and `Message(sender, content, round_idx=0,
  msg_type="turn")`, and `Conversation.say(speaker, content, round_idx=0,
  msg_type="turn")`, are public signatures.

Improvement candidates, as proposals:

- Make `msg_type` an enum with a serde string representation, so a typo is a
  compile error rather than a turn that renders but never matches. Success
  check: the `"final_summary"` match sites compile against the enum and the
  snapshot round-trip test still passes on an existing file.
- A borrowed rendering for `render()` would avoid allocating the whole
  message list per provider call; it needs the turn list to be stable across
  the call, which compaction breaks. Success check: a benchmark on a long
  session shows the allocation gone with `compaction_e2e` still green.

## Open Gaps / Roadmap

- `render()` allocates the whole message list on every provider call. For a long
  session that is measurable; a borrowed rendering would need the turn list to be
  stable across the call, which compaction breaks.
- Message type is a free string. The values in use are conventional, not
  enumerated, so a typo produces a turn that renders but is never matched.
