---
eatmycode_version: "1.2.0"
---

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
about the world is exactly the part a framework cannot guess. This module owns
the trait and its closure impl; registration, narrowing, caching, and rendering
belong to the session, the harness, and the prompt assembler respectively.

## Status

`done` — `cargo test -p kerness context` passes 10 tests,
`cargo test -p kerness --test harness_contract` passes 16, and
`bindings/python/tests/test_session.py` passes 122, including the four
`TestContextSources` cases.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/context.rs` | the `ContextSource` trait and its closure impl |
| `crates/kerness/src/prompting.rs:96` | `CONTEXT_HEADER` and `context_block` |
| `crates/kerness/src/harness.rs:211` | the `context:` key and `resolve_context` |
| `crates/kerness/src/session.rs:848` | `add_context`, registration and name checks |
| `crates/kerness/src/session.rs:1492` | `resolve_context`, the once-per-agent render |
| `bindings/python/src/session.rs:235` | `PySource`, a Python callable behind the trait |

## Language and Conventions

Rust crate module, with a callable adapter in the PyO3 session binding. There
is no Python shim: `add_context` is a `Session` method and the trait is never
named from Python. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `context.rs` depends on `error.rs` only. `render` returns
  `crate::error::Result<String>` because a source may do IO; the trait is
  `Send + Sync` so an `Arc<dyn ContextSource>` can sit in `Shared`.
- A closure `Fn(&str) -> Result<String> + Send + Sync` is a `ContextSource`
  through the blanket impl (`crates/kerness/src/context.rs:41`), the same
  bargain the access approver makes; enforced by
  `a_closure_is_a_context_source` (`:56`).
- The Python adapter `PySource` (`bindings/python/src/session.rs:235`) calls
  the callable with the agent name, reads `str()` of the answer, and converts
  a raised exception through `Catch` ([errors.md](errors.md)), so a raising
  source stops the run as a framework error.
- Tests: the Rust prompting tests use `index_of` ordering assertions
  (`crates/kerness/src/prompting.rs:435`); the integration tests build a
  gameplan on disk with `TempDir` and drive `ScriptedProvider` from
  `crates/kerness/tests/common/mod.rs`; the Python tests use
  `SequenceMockProvider` and assert on `provider.calls`.

## Design and Invariants

Four modules share the feature, and the dependency runs one way: `context.rs`
defines the trait; `harness.rs` narrows the declared names against what was
registered; `session.rs` registers sources, resolves them once per agent, and
caches the text in `Shared.context_cache` (`crates/kerness/src/session.rs:367`);
`prompting.rs` renders the cached pairs. Nothing below the session knows a
source exists.

### Once per agent, at the top of the run

`resolve_context` (`crates/kerness/src/session.rs:1492`) calls every permitted
source once for every agent, drops blank answers, and caches the rest. The call
site is preparation (`session.rs:1020`), after tool narrowing and before persona
resolution. Two consequences, both deliberate:

- A source that walks a tree or queries a service pays for it once per agent per
  run, not once per prompt. `PromptAssembler` is rebuilt on every turn, so a
  source called from the assembler would be called several times a turn.
  Enforced by `test_a_source_is_asked_once_per_agent_and_lands_under_its_name`
  (`bindings/python/tests/test_session.py:949`).
- A source that fails stops the run before the first provider call, alongside
  persona, skill, and tool resolution. A configuration error costs nothing.
  Enforced by `a_context_source_that_fails_stops_the_run_before_any_provider_call`
  (`crates/kerness/tests/harness_contract.rs:391`) and its Python counterpart at
  `bindings/python/tests/test_session.py:987`.

### Narrowing, and the framing it does not carry

Context narrows like tools and unlike skills: a gameplan may name fewer sources
than were registered, never more, because a name nobody registered is a name for
nothing. `HarnessSpec::resolve_context` (`crates/kerness/src/harness.rs:242`)
and `resolve_tools` share one body, `narrow` (`:257`), differing only in the
noun and the remedy the refusal names. A gameplan that declares a source the
session did not register is refused by `validate_harness` (`:353`). Enforced
by `the_context_key_narrows_what_was_registered` (`:1034`),
`an_unknown_context_source_is_an_error_and_says_how_to_register_one` (`:1055`),
and `a_gameplan_narrows_the_registered_context_sources`
(`crates/kerness/tests/harness_contract.rs:343`).

The rendered text carries no quoting caveat, which is the opposite of what
[memory.md](memory.md) gets. The memory file is written by agents, so its block
says plainly that it is recorded material with no authority. A context source is
a function the host program registered — whatever it returns is what the program
that started the session chose to put in front of the model. Repeating the
memory caveat here would teach agents to discount both. A source that renders
untrusted input is responsible for framing it, and `context.rs:21` says so.
Enforced by `every_context_block_arrives_under_its_own_name`
(`crates/kerness/src/prompting.rs:421`), which asserts the block does not carry
`MEMORY_CAVEAT`.

### Invariants

- **Names are required and unique.** `add_context` refuses an empty name and a
  duplicate (`crates/kerness/src/session.rs:848`), because the name becomes the
  `###` subheading and the key a gameplan narrows by. Tested from Python only
  (`test_a_name_must_be_given_and_must_be_unique`,
  `bindings/python/tests/test_session.py:1004`).
- **A blank answer costs no prompt.** Dropped in `resolve_context`
  (`session.rs:1503`) and again in `context_block`
  (`crates/kerness/src/prompting.rs:110`), so the cache holds what an agent
  actually sees. Enforced by `context_with_nothing_in_it_renders_nothing`
  (`prompting.rs:409`).
- **Order is registration order, and context precedes skills.** The block sits
  after the base prompt and before the skills index it is background for.
  Enforced by `context_precedes_the_skills_it_is_background_for`
  (`prompting.rs:435`).
- **The cache is read per turn, never re-rendered.** `Shared::context_for`
  (`session.rs:389`) hands the assembler the cached pairs through
  `with_context` (`prompting.rs:209`); a source whose subject changes mid-run
  reports the state it had at the top of the run.

## Key Types and Entry Points

- `crates/kerness/src/context.rs:33` — `ContextSource` — one method,
  `render(agent) -> Result<String>`, returning the text that agent should see
  or an empty string to contribute nothing. The agent's name is passed so one
  source can hand a reviewer and an author different views of the same subject.
- `crates/kerness/src/context.rs:41` — the blanket impl over
  `Fn(&str) -> Result<String>`, which is what lets a caller pass a closure where
  the signature asks for the trait.
- `crates/kerness/src/session.rs:848` — `Session::add_context(name, source)` —
  registration; `Error::Session` on an empty or duplicate name. Returns
  `&mut Self` for chaining.
- `crates/kerness/src/session.rs:1492` — `resolve_context(permitted)` — the
  once-per-agent render into `Shared.context_cache`; the first source error
  aborts preparation.
- `crates/kerness/src/prompting.rs:107` — `context_block(entries)` — renders the
  `## Context` section from `(name, text)` pairs, or an empty string when every
  entry is blank.
- `crates/kerness/src/prompting.rs:96` — `CONTEXT_HEADER` — the section opener,
  naming the text as the session's own material.
- `crates/kerness/src/harness.rs:211` — `HarnessSpec::context` — the gameplan's
  `context:` key; `None` means every registered source, a list narrows.
- `crates/kerness/src/harness.rs:342` — `Permitted` — what `validate_harness`
  hands back: `tools` and `context`, two narrowed lists in registration order,
  named rather than positional so a caller cannot read one for the other.
- `bindings/python/src/session.rs:582` — `Session.add_context(name, source)` —
  the pyfunction wrapping any callable in `PySource`.

## Interactions

- Rendered into the system prompt by [prompting.md](prompting.md), ahead of the
  skills index it is background for; the shared shape is `Vec<(String, String)>`
  of `(name, text)` pairs, supplied through `PromptAssembler::with_context`.
- Registered on, resolved by, and cached in [session.md](session.md);
  `Shared.context_cache` is keyed by agent name and read under the shared lock.
- Declared and narrowed through [harness.md](harness.md)'s `context:` key;
  `validate_harness` receives the registered names and returns `Permitted`.
- Counted as prompt overhead by [compaction.md](compaction.md), because the
  block is part of the system message.
- Raises through [errors.md](errors.md): a Python source's exception crosses as
  a framework error through `Catch`, so `except RuntimeError` from a raising
  source is not guaranteed — the integration test matches on the message.
- Integration is tested in `crates/kerness/tests/harness_contract.rs` and
  `bindings/python/tests/test_session.py`'s `TestContextSources`.

## How to Test

```sh
cargo test -p kerness context                                       # pass = 10 passed, 0 failed
cargo test -p kerness --test harness_contract                       # pass = 16 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q  # pass = 122 passed
```

- `crates/kerness/src/context.rs:56` — `a_closure_is_a_context_source` — the
  blanket impl, which is the whole ergonomic claim.
- `crates/kerness/src/prompting.rs:409` — `context_with_nothing_in_it_renders_nothing`,
  `:421` `every_context_block_arrives_under_its_own_name`, and `:435`
  `context_precedes_the_skills_it_is_background_for` — `context_block`, the
  absent caveat, and the fixed part order. These are pure string assembly with
  no state and no IO, so they are asserted once, here; no Python test covers
  `context_block`.
- `crates/kerness/src/harness.rs:1034` — `the_context_key_narrows_what_was_registered`
  — the three states of the key, and `:1055`
  `an_unknown_context_source_is_an_error_and_says_how_to_register_one` — the
  refusal naming `session.add_context(...)`.
- `crates/kerness/tests/harness_contract.rs:315` —
  `a_declared_context_source_that_is_not_registered_is_refused_with_the_list`,
  `:343` `a_gameplan_narrows_the_registered_context_sources`, and `:391`
  `a_context_source_that_fails_stops_the_run_before_any_provider_call` — the
  contract driven through a whole session.
- `bindings/python/tests/test_session.py:915` — `TestContextSources` — asked
  once per agent and landing under its name (`:949`), a declared source nobody
  registered stopping the run with no provider call (`:975`), and a raising
  source stopping the run before any provider call (`:987`).
- Gap: `add_context`'s duplicate-name refusal has no Rust unit test; only
  `bindings/python/tests/test_session.py:1004` drives it.

## Review and Refactor Guide

- **Changing the trait signature** (`render`) → inspect `PySource`
  (`bindings/python/src/session.rs:239`), the blanket impl, and every closure
  in `crates/kerness/tests/harness_contract.rs`; the Python callable contract
  `source(agent_name) -> str` is public and must stay one-argument.
- **Changing when sources render** → `resolve_context`'s call site in
  preparation (`crates/kerness/src/session.rs:1020`) and the cache in `Shared`;
  a re-render point would need to invalidate `context_cache` and is tied to
  the step machine in [run.md](run.md). `test_a_source_is_asked_once_per_agent_and_lands_under_its_name`
  pins the current count.
- **Changing the rendered shape** (`CONTEXT_HEADER`, subheadings, order) →
  the three prompting tests at `crates/kerness/src/prompting.rs:409`, `:421`,
  `:435`, and `prompt_overhead` in [compaction.md](compaction.md), which
  measures the result.
- **Changing narrowing** → `narrow` in `crates/kerness/src/harness.rs:257` is
  shared with tools; a change there changes both refusals and both tests at
  `:1034` and `:1055`.
- **Safe extension points**: a new kind of source is a new impl of the trait
  outside the crate; the framework ships none and should ship none.
- **Forbidden coupling**: `context.rs` must not import the session, the prompt
  assembler, or the harness; sources are `Arc<dyn ContextSource>` owned by
  `Session`, never by an `Agent`.
- **Compatibility**: `Permitted { tools, context }` is a public struct with
  named fields; the `context:` frontmatter key's three states (`None`, list,
  `[]`) are part of the gameplan contract.

Improvement candidates, as proposals:

- Add a Rust unit test for `add_context`'s duplicate-name refusal beside the
  `add_tool` one (`crates/kerness/src/session.rs:2641`). Success check: the
  test asserts `Error::Session` naming the duplicate without the Python
  surface installed.
- A per-source character budget would let the session refuse an oversized
  block before compaction counts it as overhead. Success check: a source
  returning more than the budget is refused at preparation with the figure.

## Open Gaps / Roadmap

- No budget. A source that returns a megabyte returns a megabyte, and
  [compaction.md](compaction.md) counts it as prompt overhead the history has to
  fit inside rather than as something to shrink. Bounding it would mean choosing
  a truncation the framework cannot choose well — the source knows what is worth
  keeping and the framework does not.
- Rendered once per run. A source whose subject changes mid-session — a file the
  agents are editing — reports the state it had at the top of the run. Re-render
  points belong with the event and step machine on the
  [root roadmap](../ARCHITECTURE.md#roadmap), where the caller decides when a
  run pauses.
