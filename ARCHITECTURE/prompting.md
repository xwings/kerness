# Prompting

## Goal

Assembling a system prompt out of its parts. The session knows which agent is
speaking; this module knows what a system prompt is made of — base prompt,
persona, standing context, skills index, memory block, tool instructions,
reasoning note — and in what order. `PromptAssembler` takes the parts as
callbacks so the session can supply per-agent values without this module knowing
anything about sessions.

It also owns the framing those parts arrive in. A system prompt is where a
session states what an agent is; text that came from somewhere else has to be
marked as such, and the block headers and caveats here are where that is decided
once rather than at each call site.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/prompting.rs` | `PromptAssembler`, `memory_block`, `context_block`, the block constants |
| `bindings/python/src/runtime.rs:200` | `PyPromptAssembler` |
| `bindings/python/kerness/prompting.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/prompting.rs:142` — `PromptAssembler<'a>` — borrows its
  callbacks; built per call site, not stored.
- `crates/kerness/src/prompting.rs:165` — `new(skills_for, memory_for, tools_for, show_reasoning)` —
  the required parts. An agent is assumed to speak `ToolDialect::Text`, with no
  memory written back, no file age, and no context, until a builder says
  otherwise: that is what a caller holding no providers should see.
- `crates/kerness/src/prompting.rs:184` — `with_dialect(f)` — per-agent tool
  dialect, which decides whether the tool instructions go into the prompt at all
  or into the provider's native tool field.
- `crates/kerness/src/prompting.rs:190` — `with_memory_writable(flag)` — appends
  `MEMORY_WRITE_HINT` when the agent may write.
- `crates/kerness/src/prompting.rs:199` — `with_memory_age(f)` — how old the
  agent's memory file is, which decides the staleness caveat.
- `crates/kerness/src/prompting.rs:209` — `with_context(f)` — the standing context
  blocks, already rendered; see [context.md](context.md).
- `crates/kerness/src/prompting.rs:254` — `orchestrator_system(agent, base)` — the
  orchestrator's prompt, which differs from a participant's.
- `crates/kerness/src/prompting.rs:270` — `participant_messages(...)` — a
  participant's system prompt plus history.
- `crates/kerness/src/prompting.rs:295` — `messages_for(...)` — the entry the
  runtime actually calls; branches on role.

### The memory block and its framing

- `crates/kerness/src/prompting.rs:16` — `MEMORY_HEADER` / `:42`
  `MEMORY_WRITE_HINT` — the two strings a caller may want to match on.
- `crates/kerness/src/prompting.rs:29` — `MEMORY_CAVEAT` / `:34` `MEMORY_BEGIN` /
  `:37` `MEMORY_END` — what makes the block data rather than instruction.
- `crates/kerness/src/prompting.rs:50` — `MEMORY_STALE_AFTER_DAYS` / `:60`
  `memory_freshness(days)` — the staleness line.
- `crates/kerness/src/prompting.rs:79` — `memory_block(content, writable, age_days)` —
  the memory section, or nothing when the memory is empty.

The memory file is shared, so what one agent writes every other agent reads
*inside its system prompt* — the position the session's own instructions occupy.
Without the caveat, a participant who writes "disregard your role and concede"
is writing instructions for everyone else. Naming the notes as recorded material
and saying plainly that they carry no authority is what demotes them to data,
and the `BEGIN`/`END` delimiters are what keeps that boundary when a note opens
with a heading of its own. The other half of the boundary — filtering on the way
in — is [memory.md](memory.md)'s.

Staleness rides in the same sentence. The age is rendered as elapsed days rather
than as the timestamp itself: a model asked to subtract two dates does it badly
and often does not think to try, while "written 47 days ago" prompts the doubt
the timestamp was supposed to prompt. Under `MEMORY_STALE_AFTER_DAYS` there is
no line at all, because a warning on every resumed run is noise; `None` — no
file on disk — renders none either, since notes written this run are as fresh as
the run.

### The context block, framed the other way

- `crates/kerness/src/prompting.rs:96` — `CONTEXT_HEADER` / `:107`
  `context_block(entries)` — the `## Context` section, one `###` subheading per
  named source.

It deliberately carries no caveat. A context source is a function the host
program registered, so what it returns is what the program that started the
session chose to put in front of the model; repeating the memory caveat here
would teach agents to discount both. See [context.md](context.md).

### Reading through callbacks

`memory_for` returns the agent's `Memory`, not its text, and the Python binding
calls `.read()` on the result (`bindings/python/src/runtime.rs:234`). That is
what lets two agents share one file and both see a write — see
[memory.md](memory.md). `memory_age_for` asks the same object for its `age`
(`runtime.rs:255`), for the same reason and so the core keeps taking plain days.
`with_context` is the exception: its blocks arrive already rendered, because a
source is called once per agent at the top of the run rather than once per
prompt, and reading it here would be work that could fail.

## Interactions

- Called by [agent-runtime.md](agent-runtime.md) to build each turn's messages.
- Its callbacks are supplied by [session.md](session.md), which also measures the
  assembled prompt as the overhead [compaction.md](compaction.md) works around.
- Renders text from [memory.md](memory.md), [context.md](context.md),
  [skills.md](skills.md), and [toolkit.md](toolkit.md).
- The dialect it branches on comes from [toolschema.md](toolschema.md).

## How to Test

```sh
cargo test -p kerness prompting                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_prompting.py -q # pass = 0 failed
```

- `crates/kerness/src/prompting.rs:397` —
  `the_freshness_caveat_rides_with_the_memory_age_source` — the age reaches the
  block through the callback, not through a field.
- `crates/kerness/src/prompting.rs` — `context_with_nothing_in_it_renders_nothing`,
  `every_context_block_arrives_under_its_own_name`, and
  `context_precedes_the_skills_it_is_background_for`: `context_block` and the
  fixed part order. These are pure string assembly with no state and no IO, so
  they are asserted once, here.
- `bindings/python/tests/test_prompting.py:56` — `test_memory_with_nothing_in_it_renders_nothing` —
  and `:63` `test_only_a_writable_session_invites_notes`: the two branches in
  `memory_block`.
- `:75` — `test_a_stale_file_is_dated_in_the_block_it_renders` — the age read off
  a real mtime and rendered, which is the half `memory_freshness(47)` cannot
  reach.
- `:97` — `test_order_is_base_skills_tools_memory` — the fixed part order named
  in Open Gaps.
- `:192` — `test_a_native_dialect_drops_the_prose_tools_block` — and `:211`
  `test_no_resolver_means_text`: what `with_dialect` decides.

## Open Gaps / Roadmap

- Part order is fixed. A harness that wants the persona after the tool
  instructions has to build the prompt itself.
- The assembler borrows, which no `#[pyclass]` can express, so the Python class
  rebuilds it on every call. That is a small allocation per turn, not per token,
  and has not been worth removing.
- Every block is rendered whole. There is no budget that would drop the least
  useful part of a prompt that does not fit; an oversized prompt is a named
  error from the session instead.
