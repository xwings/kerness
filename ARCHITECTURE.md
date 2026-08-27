# Kerness

## Mission

Kerness is a framework for building **multi-agent harnesses**: sessions in which
several language models hold a structured conversation, call tools, consult a
shared memory file, and produce a result with named fields.

The organising idea is that a **Markdown gameplan is the program**. Its YAML
frontmatter is a machine-readable contract — who the agents are, how many
rounds, which phases, which tools, what the result must contain — and its body
is the prose manual the orchestrator reads. A harness author writes Markdown;
the framework validates it, assembles the prompts, drives the loop, dispatches
the tools, and enforces the access boundary.

Two properties follow from that and shape every decision below:

- **The contract is total.** Every key the frontmatter parser accepts is
  validated, rendered into a prompt, or enforced at runtime. A key that parses
  and then does nothing is a bug, not a reserved word.
- **Everything is synchronous.** There is no executor, no async runtime, and no
  hidden concurrency. A session runs on the calling thread, and a stack trace
  from inside a tool handler reaches all the way back to `Session::run`.

Kerness ships as two artifacts from one repository: a **Rust crate** for callers
who want the framework in a Rust program, and a **Python extension** for callers
who want to subclass `Provider`, pass a lambda as a tool handler, and hand a
`pydantic` model in for structured output. Both are first-class; neither is a
wrapper around the other's use case.

## Target Environment

| | |
| --- | --- |
| Rust | MSRV **1.88**; stable toolchain; 2021 edition |
| Python | **3.10+**, CPython, via the stable ABI (`abi3-py310`) |
| Bindings | `pyo3` 0.23 with `extension-module` |
| Build | `cargo` for the crate, `maturin` for the wheel |
| Platform | Any target `ureq` + `rustls` supports; developed on Linux x86-64 |
| Network | Outbound HTTPS only, to provider endpoints the caller names |
| Runtime deps | None beyond the crate's Cargo dependencies; `pydantic` is optional and only for structured output |

There is no daemon, no database, no listening socket, and no background thread.
Filesystem writes are confined to paths the caller opts into: the memory file,
the session file, channel logs, and directories added to the access policy.

A built wheel is tagged `kerness-0.1.0-cp310-abi3-<platform>` — one wheel per
platform covers every supported Python.

## Workspace Layout

```
Cargo.toml                  workspace root, shared dependency versions
crates/
  kerness/                  the framework — pure Rust, links no Python
    src/                    24 modules (see the Index)
    assets/                 built-in gameplans, personas, skills
    tests/                  88 integration tests over the crate's public surface
    examples/               8 harnesses driven from Rust alone
  kerness-py/               the Python extension — thin, decides nothing
    src/                    11 modules, one per boundary concern
python/
  kerness/                  the installed Python package
    __init__.py             bootstrap + public surface
    *.py                    per-subsystem re-export shims
    provider.py             deliberate Python (see ARCHITECTURE/provider.md)
    access.py               deliberate Python (console prompt)
    selfcheck.py            deliberate Python (import health)
    gameplans/ personas/ skills/   the same assets, installed
tests/                      26 pytest modules over the Python surface
examples/                   runnable harnesses, exercised by tests/test_examples.py
.github/workflows/          CI on every push; release builds wheels and an sdist
assets/                     project marks: logo.svg, logo-mark.svg
README.md                   the public introduction
LICENSE                     MIT
ARCHITECTURE.md             this file
ARCHITECTURE/               one file per subsystem
```

`crates/kerness/assets/` and `python/kerness/{gameplans,personas,skills}/` hold
byte-identical copies. Both must exist: the crate cannot read the package's copy
when used from Rust alone, and the wheel cannot ship the crate's. Nothing in the
build keeps them in step, so `tests/test_packaging.py:30` asserts it directly.

## Boot and Entry Flow

### From Python

1. `import kerness` runs `python/kerness/__init__.py`.
2. Line 12 calls `_core.bootstrap(exceptions, _enums.ToolDialect, <package dir>)`.
   The extension cannot declare three things itself, so they are handed down:
   the exception classes (two-argument constructors a `create_exception!` cannot
   express), the `ToolDialect` enum (callers compare members with `is`, so it
   must be a real `enum.Enum`), and the assets root (only the package knows where
   pip put it). `bootstrap` also installs the HTTP transport seam and the logger
   — `crates/kerness-py/src/lib.rs:33`.
3. The remaining imports pull the public names out of the per-subsystem shims.
4. The caller builds a `Session(...)`, registers participants, tools, and skills,
   and calls `run()`.

### From Rust

1. The caller sets `kerness::assets::set_root(...)` if the built-in gameplans are
   wanted from outside the crate directory, otherwise `$KERNESS_ASSETS` or
   `$CARGO_MANIFEST_DIR/assets` resolves it — `crates/kerness/src/assets.rs:31`.
2. `SessionConfig { .. }` → `Session::new` → `add_participant` → `run`.

### Inside `run()`

`Session::run` (`crates/kerness/src/session.rs:566`) is the whole harness:
it resolves the gameplan's harness spec against what was registered, builds the
skill registry and access manager, seeds the `Conversation`, then either runs the
round-robin participant loop or hands control to `OrchestratorLoop::run`
(`crates/kerness/src/orchestrator.rs:447`) when the gameplan declares an
orchestrator. Each agent turn goes through `AgentRunner::run`
(`crates/kerness/src/agent_runtime.rs:110`), which is the provider-call /
tool-call / feed-results cycle. A `SessionResult` comes back with the transcript,
the phase reached, the end reason, and the parsed result fields.

## Well-Known Constants

Values callers see in output or depend on in tests. Each is exported from the
Python package as well as the crate.

| Constant | Value | Owner |
| --- | --- | --- |
| `SCHEMA_VERSION` | `1` | `crates/kerness/src/sessionfile.rs:33` |
| `DEFAULT_MAX_CONTEXT_TOKENS` | `256_000` | `crates/kerness/src/session.rs:54` |
| `CHARS_PER_TOKEN` | `4` | `crates/kerness/src/compaction.rs:33` |
| `COMPACT_TO_FRACTION` | `0.5` | `crates/kerness/src/compaction.rs:40` |
| `MAX_INVALID_CALLS` | `3` | `crates/kerness/src/agent_runtime.rs:33` |
| `DEFAULT_TERMINATORS` | `CONSENSUS_REACHED`, `END_SESSION` | `crates/kerness/src/utils.rs:12` |
| `RESERVED_TOOL_NAMES` | `["Skill"]` | `crates/kerness/src/harness.rs:25` |
| `DEFAULT_TIMEOUT` | 60s | `crates/kerness/src/exec.rs:18` |
| Provider base URLs | OpenAI, OpenRouter, Anthropic | `crates/kerness/src/provider/{openai,openrouter,claude}.rs:17,15,15` |

## Roadmap

| | Milestone | Status |
| --- | --- | --- |
| **M1** | Core primitives: errors, utils, HTTP, JSON Schema, tooling, tool schemas, toolkit, conversation, memory, compaction, personas, channels | done |
| **M2** | Contract and runtime: harness, gameplans, access, exec, skills, agents, prompting, session files, providers, agent runtime, orchestrator loop, session | done |
| **M3** | Python surface: the extension module, the base classes Python subclasses, the patchable transport seam, the package and its assets | done |
| **M4** | Test suite and examples: 26 pytest modules over the Python surface, runnable example harnesses | done |
| **M5** | Surface sweep: no unused dependency, no unreferenced `pub` item, clippy clean at `-D warnings`, every framework constant reachable from Python | done |
| **M6** | This architecture doc set, and the public introduction: `README.md`, project marks, MIT licence | done |
| **M7** | Release verification: full test matrix green, wheel and sdist build, the sdist installs into a clean interpreter and its self-check exits 0 | done |
| **M8** | The Rust surface proving itself: 88 integration tests over the public API, an example per capability including one that needs no key, and CI running both suites on every push | done |

## Verification

The commands that gate a change. Each module file names the subset that proves
its own status.

```sh
cargo fmt --all -- --check                         # pass = exit 0
cargo test --workspace -q                          # pass = 305 unit + 88 integration, 0 failed
cargo clippy --workspace --all-targets -- -D warnings   # pass = exit 0
cargo build -p kerness --examples                  # pass = all 8 compile
cargo run -p kerness --example offline_debate      # pass = completes with no key, no network
.venv/bin/maturin develop                          # pass = "Installed kerness-0.1.0"
.venv/bin/python -m pytest tests/ -q               # pass = 394 passed
.venv/bin/python -m kerness.selfcheck              # pass = "OK: all core checks passed", exit 0
```

`.github/workflows/ci.yml` runs all of the above on every push, plus
`cargo doc --no-deps -p kerness` under `RUSTDOCFLAGS=-D warnings`,
`cargo check --workspace --all-targets --locked` on the MSRV toolchain, and
`ruff check`. See [testing.md](ARCHITECTURE/testing.md).

`maturin` and `python` are not on `PATH` in this workspace; invoke them from
`.venv/bin/`.

## Coding Discipline

Behavioral guidelines to reduce common LLM coding mistakes. Merge with
project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For
trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If
yes, simplify.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:

```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make
it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs,
fewer rewrites due to overcomplication, and clarifying questions come
before implementation rather than after mistakes.

### Project-Specific Deviations

- **Dead configuration keys are defects.** Every field the harness parser
  accepts must be validated, rendered into a prompt, or enforced at runtime.
  There are no reserved harness keys held for later.
- **The docs describe the code as it is.** No changelog prose, no milestone
  labels in comments, no "used to" or "changed in": a comment or document that
  narrates history describes something a reader cannot run. Removals are
  outright, and what replaced them is documented on its own terms.
- **Bundled assets remain framework-generic.** Domain-specific gameplans,
  personas, and skills live with the project that owns their domain, not in
  `assets/`.
- **Inventory tests assert discovery, not literals.** Built-in assets and core
  modules are enumerated from disk so an addition or removal cannot silently
  escape the self-check.
- **Security boundaries receive direct tests.** Access rules, path traversal,
  symlink escape, skill bundle grants, and denied tool calls are tested at the
  layer that owns them, not only through a session that happens to exercise them.
- **Anything that touches IO returns `crate::error::Result`.** A function that
  reads a file, writes one, or calls a provider does not panic and does not
  return a bare value; the error type is the crate's own.

## Index

- [access.md](ARCHITECTURE/access.md) — the permission boundary for commands, paths, and directories.
- [agent.md](ARCHITECTURE/agent.md) — a participant or orchestrator, and its system prompt.
- [agent-runtime.md](ARCHITECTURE/agent-runtime.md) — one agent turn: call, tool-call, feed results, repeat.
- [bindings.md](ARCHITECTURE/bindings.md) — the Rust/Python boundary and the installed package.
- [channel.md](ARCHITECTURE/channel.md) — where a session's messages are delivered.
- [compaction.md](ARCHITECTURE/compaction.md) — the context ceiling and the summarize-the-prefix rewrite.
- [conversation.md](ARCHITECTURE/conversation.md) — turns, transcript, and what a provider actually sees.
- [errors.md](ARCHITECTURE/errors.md) — the error enum and its Python exception hierarchy.
- [gameplan.md](ARCHITECTURE/gameplan.md) — loading a Markdown gameplan and resolving built-in assets.
- [harness.md](ARCHITECTURE/harness.md) — the frontmatter contract: parse, validate, resolve.
- [jsonschema.md](ARCHITECTURE/jsonschema.md) — strict-mode schemas and argument validation.
- [loop.md](ARCHITECTURE/loop.md) — the orchestrator loop, phases, and end reasons.
- [memory.md](ARCHITECTURE/memory.md) — the shared Markdown file agents read and append to.
- [persona.md](ARCHITECTURE/persona.md) — loading a persona file into prompt text.
- [prompting.md](ARCHITECTURE/prompting.md) — assembling a system prompt from its parts.
- [provider.md](ARCHITECTURE/provider.md) — talking to a model, and the four built-in backends.
- [selfcheck.md](ARCHITECTURE/selfcheck.md) — `python -m kerness.selfcheck`, the installation health check.
- [session.md](ARCHITECTURE/session.md) — the top-level object that assembles and runs everything.
- [sessionfile.md](ARCHITECTURE/sessionfile.md) — saving and resuming a run.
- [skills.md](ARCHITECTURE/skills.md) — loading skill bundles and the `Skill` tool.
- [testing.md](ARCHITECTURE/testing.md) — the three suites, the examples, and what CI runs.
- [toolkit.md](ARCHITECTURE/toolkit.md) — tool specs, parsing calls out of text, dispatching them.
- [toolschema.md](ARCHITECTURE/toolschema.md) — native tool dialects and their wire shapes.
- [utils.md](ARCHITECTURE/utils.md) — text scanning, retry, and Python-compatible formatting.
