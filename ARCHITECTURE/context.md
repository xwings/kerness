# Context

## Goal

Standing background an agent reads before the conversation starts: the layout of
a repository, the rows of a table, the state of a deployment. A gameplan says
what a session is *for*; a context source says what it is *about*.

The two arrive in the prompt from different places, and that is the whole
distinction:

| | is | supplied by |
| --- | --- | --- |
| gameplan | a Markdown file with a frontmatter contract | the harness author |
| context source | a function returning text, called per agent | the host program |

The framework ships no implementations. What a session's agents need to know
about the world is exactly the part a framework cannot guess.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/context.rs` | the `ContextSource` trait and its closure impl |
| `crates/kerness/src/prompting.rs:96` | `CONTEXT_HEADER` and `context_block` |
| `crates/kerness/src/session.rs:735` | `add_context`, registration and name checks |
| `crates/kerness/src/session.rs:1308` | `resolve_context`, the once-per-agent render |
| `bindings/python/src/session.rs` | `add_context`, wrapping a Python callable |

## Key Types and Entry Points

- `crates/kerness/src/context.rs:33` — `ContextSource` — one method, `render(agent)`,
  returning the text that agent should see. The agent's name is passed so one
  source can hand a reviewer and an author different views of the same subject.
- `crates/kerness/src/context.rs:41` — the blanket impl over `Fn(&str) -> Result<String>`,
  which is what lets a caller pass a closure where the signature asks for the
  trait, as the access approver does.
- `crates/kerness/src/session.rs:735` — `Session::add_context(name, source)` — a name
  is required and must be unique; it becomes the `###` subheading the block is
  rendered under, because a model given two unlabelled blocks cannot say which
  one it is quoting.
- `crates/kerness/src/prompting.rs:107` — `context_block(entries)` — renders the
  `## Context` section. A source that returned nothing this run is skipped, so
  it costs its own call and no prompt.
- `crates/kerness/src/harness.rs:211` — `HarnessSpec::context` — the gameplan's
  `context:` key.
- `crates/kerness/src/harness.rs:342` — `Permitted { tools, context }` — what
  `validate_harness` hands back: two narrowed lists, named rather than
  positional, so a caller cannot read one for the other.

### Once per agent, at the top of the run

`resolve_context` (`session.rs:1308`) calls every permitted source once for every
agent and caches the result in `Shared.context_cache`. Two consequences, both
deliberate:

- A source that walks a tree or queries a service pays for it once per agent per
  run, not once per prompt. `PromptAssembler` is rebuilt on every turn, so a
  source called from the assembler would be called several times a turn.
- A source that fails stops the run before the first provider call, alongside
  persona, skill, and tool resolution. A configuration error costs nothing.

### Narrowing, and the framing it does not carry

Context narrows like tools and unlike skills: a gameplan may name fewer sources
than were registered, never more, because a name nobody registered is a name for
nothing. A gameplan that declares a source the session did not register is
refused by `validate_harness`.

The rendered text carries no quoting caveat, which is the opposite of what
[memory.md](memory.md) gets. The memory file is written by agents, so its block
says plainly that it is recorded material with no authority. A context source is
a function the host program registered — whatever it returns is what the program
that started the session chose to put in front of the model. Repeating the
memory caveat here would teach agents to discount both. A source that renders
untrusted input is responsible for framing it, and `context.rs:21` says so.

## Interactions

- Rendered into the system prompt by [prompting.md](prompting.md), ahead of the
  skills index it is background for.
- Registered on, resolved by, and cached in [session.md](session.md).
- Declared and narrowed through [harness.md](harness.md)'s `context:` key.

## How to Test

```sh
cargo test -p kerness context                                       # pass = 0 failed
cargo test -p kerness --test harness_contract                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q  # pass = 0 failed
```

- `crates/kerness/src/context.rs:56` — `a_closure_is_a_context_source` — the blanket
  impl, which is the whole ergonomic claim.
- `crates/kerness/tests/harness_contract.rs` — a gameplan narrowing the registered
  sources, and a declared source nobody registered refused before the run.
- `bindings/python/tests/test_session.py:803` — `TestContextSources` — asked once per
  agent, narrowed by the gameplan, and a raising source stopping the run before
  any provider call.
- `bindings/python/tests/test_prompting.py` — `TestContextBlock`, including that the
  block does not carry the memory caveat.

## Open Gaps / Roadmap

- No budget. A source that returns a megabyte returns a megabyte, and
  [compaction.md](compaction.md) counts it as prompt overhead the history has to
  fit inside rather than as something to shrink. Bounding it would mean choosing
  a truncation the framework cannot choose well — the source knows what is worth
  keeping and the framework does not.
- Rendered once per run. A source whose subject changes mid-session — a file the
  agents are editing — reports the state it had at the top of the run. Re-render
  points belong with the event and step machine on the root roadmap, where the
  caller decides when a run pauses.
