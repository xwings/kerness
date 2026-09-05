# Conversation

## Goal

The session's memory of what has been said, in two shapes at once. `turns` is
the structured record — who spoke, in which round, of what kind — and
`transcript` is the flat message list a harness prints or saves. `render()`
turns the first into the `ChatMessage` list a provider is actually sent.

Keeping both is what lets compaction rewrite the history without losing the
transcript, and lets a resumed session restore each independently.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/conversation.rs` | `ChatMessage`, `Message`, `Turn`, `Conversation` |
| `bindings/python/src/runtime.rs` | `PyConversation` |
| `bindings/python/src/types.rs` | `PyMessage` (`:488`), `PyTurn` (`:567`) |
| `bindings/python/kerness/conversation.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/conversation.rs:16` — `ChatMessage` — role and content; the
  wire shape a provider takes.
- `crates/kerness/src/conversation.rs:36` — `Message` — sender and content; the
  transcript shape a human reads.
- `crates/kerness/src/conversation.rs:58` — `Turn` — a speaker, a round index, and
  a message type; `render()` at `:91` is how it becomes a `ChatMessage`.
- `crates/kerness/src/conversation.rs:122` — `directive(content)` — a user-role
  instruction inserted into the history.
- `crates/kerness/src/conversation.rs:136` — `say(speaker, content, round, type)` —
  the normal path: appends to both the turns and the transcript.
- `crates/kerness/src/conversation.rs:163` — `render()` — the full `ChatMessage`
  list; allocates a fresh vector, which is why the Python side exposes it as a
  method rather than a property.
- `crates/kerness/src/conversation.rs:181` — `replace_turns(turns)` — what
  compaction calls.
- `crates/kerness/src/conversation.rs:186` — `restore(turns, transcript)` — what a
  resumed session calls; the two lists are restored together because a snapshot
  holds both.

## Interactions

- Owned by [session.md](session.md), one per run.
- Rewritten by [compaction.md](compaction.md) through `replace_turns`.
- Rendered into provider calls by [agent-runtime.md](agent-runtime.md).
- Saved and restored by [sessionfile.md](sessionfile.md).

## How to Test

```sh
cargo test -p kerness conversation                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_conversation.py -q # pass = 0 failed
```

- `bindings/python/tests/test_conversation.py:25` — `test_both_accessors_hand_back_a_fresh_list` —
  `turns()` and `transcript()` are methods returning fresh lists, so mutating what
  a caller got back cannot corrupt the conversation.

## Open Gaps / Roadmap

- `render()` allocates the whole message list on every provider call. For a long
  session that is measurable; a borrowed rendering would need the turn list to be
  stable across the call, which compaction breaks.
- Message type is a free string. The values in use are conventional, not
  enumerated, so a typo produces a turn that renders but is never matched.
