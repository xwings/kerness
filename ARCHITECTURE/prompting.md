# Prompting

## Goal

Assembling a system prompt out of its parts. The session knows which agent is
speaking; this module knows what a system prompt is made of — base prompt,
persona, skills index, memory block, tool instructions, reasoning note — and in
what order. `PromptAssembler` takes the parts as callbacks so the session can
supply per-agent values without this module knowing anything about sessions.
Serves **M2**.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/prompting.rs` | `PromptAssembler`, `memory_block`, the memory constants |
| `bindings/python/src/runtime.rs:210` | `PyPromptAssembler` |
| `bindings/python/kerness/prompting.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/prompting.rs:53` — `PromptAssembler<'a>` — borrows the three
  callbacks; built per call site, not stored.
- `crates/kerness/src/prompting.rs:74` — `new(skills_for, memory_for, tools_for, show_reasoning)` —
  the required parts.
- `crates/kerness/src/prompting.rs:91` — `with_dialect(f)` — per-agent tool
  dialect, which decides whether the tool instructions go into the prompt at all
  or into the provider's native tool field.
- `crates/kerness/src/prompting.rs:97` — `with_memory_writable(flag)` — appends
  `MEMORY_WRITE_HINT` when the agent may write.
- `crates/kerness/src/prompting.rs:126` — `orchestrator_system(agent, base)` — the
  orchestrator's prompt, which differs from a participant's.
- `crates/kerness/src/prompting.rs:143` — `participant_messages(...)` — a
  participant's system prompt plus history.
- `crates/kerness/src/prompting.rs:167` — `messages_for(...)` — the entry the
  runtime actually calls; branches on role.
- `crates/kerness/src/prompting.rs:16` — `MEMORY_HEADER` / `:22`
  `MEMORY_WRITE_HINT` — the two strings a caller may want to match on.
- `crates/kerness/src/prompting.rs:30` — `memory_block(content, writable)` — the
  memory section, or nothing when the memory is empty.

The `memory_for` callback returns the agent's `Memory`, not its text; the
binding calls `.read()` on the result. That is what lets two agents share one
file and both see a write — see [memory.md](memory.md).

## Interactions

- Called by [agent-runtime.md](agent-runtime.md) to build each turn's messages.
- Its callbacks are supplied by [session.md](session.md).
- Renders text from [memory.md](memory.md), [skills.md](skills.md), and
  [toolkit.md](toolkit.md).
- The dialect it branches on comes from [toolschema.md](toolschema.md).

## How to Test

```sh
cargo test -p kerness prompting                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_prompting.py -q # pass = 0 failed
```

- `bindings/python/tests/test_prompting.py:56` — `test_memory_with_nothing_in_it_renders_nothing` —
  and `:63` `test_only_a_writable_session_invites_notes`: the two branches in
  `memory_block`.
- `:77` — `test_order_is_base_skills_tools_memory` — the fixed part order named in
  Open Gaps.
- `:172` — `test_a_native_dialect_drops_the_prose_tools_block` — and `:191`
  `test_no_resolver_means_text`: what `with_dialect` decides.

## Open Gaps / Roadmap

- Part order is fixed. A harness that wants the persona after the tool
  instructions has to build the prompt itself.
- The assembler borrows, which no `#[pyclass]` can express, so the Python class
  rebuilds it on every call. That is a small allocation per turn, not per token,
  and has not been worth removing.
