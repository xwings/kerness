# Kerness

## Mission

Kerness is a framework for building **multi-agent harnesses**: sessions in which
several language models hold a structured conversation, call tools, consult a
shared memory, and produce a result with named fields.

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
  from inside a tool handler reaches the calling `SessionRun::step` or `Session::run`.

Kerness ships as two artifacts from one repository: a **Rust crate** for callers
who want the framework in a Rust program, and a **Python extension** for callers
who want to subclass `Provider`, pass a lambda as a tool handler, and hand a
`pydantic` model in for structured output. Both are first-class surfaces;
neither is a wrapper around the other's use case.

What is not symmetrical is where the code lives. **A feature is written in
Rust.** The crate implements it, the extension exposes it, and the installed
Python package does one of five things and nothing else: declares a class
callers subclass (`Provider`, `Channel`, `MemoryStore`), declares one the
extension cannot
(the exception hierarchy's structured constructors, `ToolDialect` as a real
`enum.Enum`, `AccessPolicy` as a dataclass whose contract is written in Python
list semantics), reads a signature with `inspect`, validates with `pydantic`,
or re-exports a name. Every other `.py` in the package is a shim.

Where a feature needs something only the interpreter has — `sys.stdout`, a
logger, `input`, an HTTP client under a caller's `mock.patch` — the crate names
the need as a trait, ships a default that works from Rust alone, and the
binding installs a replacement at `bootstrap`. The behaviour stays in one place
and only its delivery crosses:

| Seam | Crate default | What the binding installs |
| --- | --- | --- |
| `HttpTransport` (`http.rs:24`) | `ureq` + `rustls` | `kerness.provider.http_post_json`, resolved per call so `mock.patch` reaches it |
| `Logger` (`logging.rs:28`) | warnings and errors to stderr | `logging.getLogger("kerness")`, so `caplog` sees them |
| `ConsoleWriter` (`channel.rs:54`) | this process's stdout | `builtins.print`, so `capsys` and a `StringIO` see it |
| `ConsolePrompt` (`access.rs:69`) | `std::io::stdin` | `sys.stdin` / `builtins.input` |

## Target Environment

| | |
| --- | --- |
| Rust | MSRV **1.88**; stable toolchain; 2021 edition |
| Python | **3.10+**, CPython, via the stable ABI (`abi3-py310`) |
| Bindings | `pyo3` 0.23 with `extension-module` |
| Build | `cargo` for the crate, `maturin` for the wheel |
| Platform | Linux and macOS; developed on Linux x86-64. `ureq` + `rustls` reach further, but path confinement resolves every path from `/` — `crates/kerness/src/access.rs:713` — so the access boundary assumes POSIX paths |
| Network | Outbound HTTPS only, to provider endpoints the caller names |
| Runtime deps | None beyond the crate's Cargo dependencies; `pydantic` is optional and only for structured output |

There is no daemon, no database, no listening socket, and no background thread.
Filesystem writes are confined to paths the caller opts into: whatever the
memory store names for a scope, the session file, channel logs, and directories
added to the access policy.

A built wheel is tagged `kerness-<version>-cp310-abi3-<platform>` — one wheel
per platform covers every supported Python, and `<version>` is whatever the root
`Cargo.toml` says.

## Workspace Layout

One artifact per top-level directory: `crates/` is the crate, `bindings/` is
everything the wheel is made of. Neither reaches into the other's tests, and the
root carries one manifest — `Cargo.toml`. A Python build starts from
`bindings/python/`, not from here.

```
Cargo.toml                  workspace root, shared dependency versions
crates/
  kerness/                  the framework — pure Rust, links no Python
    src/                    31 top-level modules plus provider/, skill/, and session/,
                            documented by subsystem in the Index
    assets/                 built-in gameplans, roles, personas, skills
    tests/                  integration tests over the crate's public surface
    examples/               10 harnesses driven from Rust alone
bindings/
  python/                   everything the wheel is built from
    pyproject.toml          the wheel's manifest; `pip install .` runs here
    Cargo.toml              the `kerness-py` crate, a workspace member
    LICENSE  README.md      symlinks to the root copies
    src/                    13 modules, one per boundary concern
    kerness/                the installed Python package
      __init__.py           bootstrap + public surface
      *.py                  per-subsystem re-export shims
      provider.py           the subclassable ABC (see ARCHITECTURE/provider.md)
      access.py             the AccessPolicy dataclass
      channel.py            the subclassable ABC
      memory.py             the subclassable ABC (see ARCHITECTURE/memory.md)
      exceptions.py         the exception hierarchy
      _enums.py             ToolDialect
      selfcheck.py          import health
      gameplans/ roles/ personas/ skills/   the same assets, installed
    tests/                  26 pytest modules over the Python surface
    examples/               runnable harnesses, walked by test_examples.py above
.github/workflows/          CI on every push; release builds wheels and an sdist,
                            then uploads them to PyPI from a `v*` tag
assets/                     project marks: logo.svg, logo-mark.svg
README.md                   the public introduction
LICENSE                     MIT
ARCHITECTURE.md             this file
ARCHITECTURE/               one file per subsystem
```

The two symlinks are load-bearing. `readme` and `license-files` in
`pyproject.toml` resolve against the directory holding it and reject a `..`
path, so without a local `LICENSE` and `README.md` the wheel builds and ships
neither the licence text nor the long description, with nothing on stderr to say
so.

Nothing under `bindings/python/` other than the package reaches the wheel —
maturin packages only the directory matching `module-name`, so `tests/` and
`examples/` sit beside it without shipping in it. The sdist is wider: maturin
roots it at the Cargo workspace, so it carries `crates/` as well, which is what
lets it build from source with no wheel available.

The version is declared once, as `[workspace.package] version` in the root
`Cargo.toml`. `pyproject.toml` is `dynamic = ["version"]`, the extension exposes
`env!("CARGO_PKG_VERSION")` as `_core.__version__`, and the package re-exports
that as `kerness.__version__`. A release bumps one number.

The release workflow publishes Python artifacts from `v*` tags through the
GitHub `pypi` environment and PyPI Trusted Publishing. The setup and release
procedure live in [README.md](README.md#releasing); build and verification
ownership lives in [testing.md](ARCHITECTURE/testing.md).

`crates/kerness/assets/` and
`bindings/python/kerness/{gameplans,roles,personas,skills}/` hold byte-identical
copies. Both must exist: the crate cannot read the package's copy when used from
Rust alone, and the wheel cannot ship the crate's. Nothing in the build keeps
them in step, so `bindings/python/tests/test_packaging.py:42` asserts it
directly.

## Boot and Entry Flow

### From Python

1. `import kerness` runs `bindings/python/kerness/__init__.py`.
2. Line 12 calls `_core.bootstrap(exceptions, _enums.ToolDialect, <package dir>)`.
   The extension cannot declare three things itself, so they are handed down:
   the exception classes (structured constructors a `create_exception!` cannot
   express), the `ToolDialect` enum (callers compare members with `is`, so it
   must be a real `enum.Enum`), and the assets root (only the package knows where
   pip put it). `bootstrap` then installs all four seams — transport, console
   writer, logger, console prompt — at `bindings/python/src/lib.rs:36`, which is
   why an import is enough and no caller wires anything.
3. The remaining imports pull the public names out of the per-subsystem shims.
4. The caller builds a `Session(...)`, registers agents, tools, and skills,
   and calls `run()` or consumes the configuration with `start()` to step it.

### From Rust

1. The caller sets `kerness::assets::set_root(...)` if the built-in gameplans are
   wanted from outside the crate directory, otherwise `$KERNESS_ASSETS` or
   `$CARGO_MANIFEST_DIR/assets` resolves it — `crates/kerness/src/assets.rs:38`.
2. `SessionConfig { .. }` → `Session::new` → registration → `start` / `run`.

### Preparation and execution

`Session::start` (`crates/kerness/src/session.rs:914`) consumes configuration,
resolves and validates the roster, prompts, skills, tools, context and access,
opens the memory scopes, and returns an owned `SessionRun`. Live agent turns
remain typed Rust values; serialization belongs to checkpoints. Its state
machine advances synchronous provider calls, individual tools, scheduler effects,
and memory maintenance through `step`. Host-driven mode accepts explicit agent
selection and validated results; automatic mode follows the gameplan and requires
an orchestrator. An explicit harness role requirement applies in both modes.

`Session::run` (`crates/kerness/src/session.rs:900`) drives this same engine with
legacy result coercion, provider error placeholders and synchronous approval
callbacks. The additive `start` API uses strict outcomes and external approvals
by default. Python forwards to these Rust entry points.

[run.md](ARCHITECTURE/run.md) owns live state, events, capabilities, cancellation,
approvals, outcomes and recovery. [session.md](ARCHITECTURE/session.md) owns
configuration and preparation. [loop.md](ARCHITECTURE/loop.md) and
[agent-runtime.md](ARCHITECTURE/agent-runtime.md) own scheduling and agent turns.
[sessionfile.md](ARCHITECTURE/sessionfile.md) owns schema validation and atomic
checkpoint publication; [access.md](ARCHITECTURE/access.md) owns POSIX command
deadlines, process-group cleanup and output draining.

## Well-Known Constants

Values callers see in output or depend on in tests. Each is exported from the
Python package as well as the crate.

| Constant | Value | Owner |
| --- | --- | --- |
| `SCHEMA_VERSION` | `2` | `crates/kerness/src/sessionfile.rs:36` |
| `DEFAULT_MAX_CONTEXT_TOKENS` | `256_000` | `crates/kerness/src/session.rs:66` |
| `CHARS_PER_TOKEN` | `4` | `crates/kerness/src/compaction.rs:33` |
| `COMPACT_TO_FRACTION` | `0.5` | `crates/kerness/src/compaction.rs:40` |
| `OVERFLOW_RETRY_FRACTION` | `0.5` | `crates/kerness/src/session.rs:77` |
| `MAX_INVALID_CALLS` | `3` | `crates/kerness/src/agent_runtime.rs:30` |
| `MAX_REPEATED_FAILURES` | `3` | `crates/kerness/src/agent_runtime.rs:34` |
| `MEMORY_STALE_AFTER_DAYS` | `1` | `crates/kerness/src/prompting.rs:50` |
| `DEFAULT_KEEP_ENTRIES` | `20` | `crates/kerness/src/memory.rs:440` |
| `DEFAULT_MEMORY_BUDGET` | `2_200` | `crates/kerness/src/memory.rs:715` |
| `ENTRY_SEPARATOR` | `§` | `crates/kerness/src/memory.rs:724` |
| `DEFAULT_ROLE_FILE` | `participant.md` | `crates/kerness/src/role.rs:66` |
| `DEFAULT_TERMINATORS` | `CONSENSUS_REACHED`, `END_SESSION` | `crates/kerness/src/utils.rs:12` |
| `RESERVED_TOOL_NAMES` | `["Skill"]` | `crates/kerness/src/harness.rs:25` |
| `DEFAULT_TIMEOUT` | 60s | `crates/kerness/src/exec.rs:21` |
| `ReasoningEffort::default()` | `high` | `crates/kerness/src/provider/mod.rs:64` |
| `DEFAULT_REQUEST_TIMEOUT_SEC` | `60` | `crates/kerness/src/provider/mod.rs:40` |
| `DEFAULT_RETRIES` | `2` | `crates/kerness/src/provider/mod.rs:42` |
| `DEFAULT_BACKOFF_SEC` | `2.0` | `crates/kerness/src/provider/mod.rs:44` |
| `DEFAULT_TEMPERATURE` | `1.0` | `crates/kerness/src/provider/mod.rs:46` |
| `DEFAULT_TOP_P` | `1.0` | `crates/kerness/src/provider/mod.rs:48` |
| `DEFAULT_CLAUDE_MAX_TOKENS` | `4096` | `crates/kerness/src/provider/claude.rs:26` |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | `crates/kerness/src/provider/openai.rs:18` |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | `crates/kerness/src/provider/openrouter.rs:15` |
| `CLAUDE_BASE_URL` | `https://api.anthropic.com/v1` | `crates/kerness/src/provider/claude.rs:16` |

The request defaults below `ReasoningEffort` are declared once and named twice:
the crate's four backends build their `Default` impls from them, and the Python
constructors write the same constants into their own signatures. A value spelled
out in both languages drifts silently; one both sides import cannot.
`crates/kerness/tests/public_api.rs` and
`bindings/python/tests/test_provider.py` each assert them.

## Roadmap

The M1–M3 core upgrade is implemented in Rust. Python exposes the required
capabilities through API bindings. Execution remains synchronous, with no hidden
executor or concurrent agent scheduling.

| Milestone | Status | Delivered behavior and evidence |
| --- | --- | --- |
| **M1 — Runtime ownership and tool capabilities** | done | Owned `SessionRun`, complete `ToolSpec` registration, contextual handlers with immutable identity and scoped capabilities. Registration, capability lifetime and resource lifecycle tests pass. |
| **M2 — Host-driven execution** | done | Shared run/step engine, typed input/events/control, external approval, single-agent host mode, schema-2 continuation, v1 boundary migration and explicit reconciliation. Equivalence, suspended approval, denial, cancellation and interrupted-action tests pass. |
| **M3 — Outcomes and budgets** | done | Strict result diagnostics, typed terminal and turn reasons, retained committed history, normalized usage and operation/tool/time admission. Token/cost limits require explicit measured-threshold mode. Retry, compaction, maintenance and nested provider accounting tests pass. |
| **M4 — Adapters and richer sessions** | deferred | Native streaming, workflow adapters, session-store listing/forking, typed content parts, MCP and sequential subagents need separately justified changes. Parallel execution requires revising the synchronous invariant. |

The public contracts and their practical limits belong to
[run.md](ARCHITECTURE/run.md). Rust examples
[`host_control`](crates/kerness/examples/host_control.rs) and
[`resume_approval`](crates/kerness/examples/resume_approval.rs), plus the
[Python host example](bindings/python/examples/host_control.py), run offline.

Compatibility is additive: existing `ToolSpec`, `ToolHandler`, `Provider` and
`SessionSnapshot` public shapes remain usable. Host callbacks and providers are
re-registered on resume; configuration is checked against the saved contract,
and `binding_version` identifies host implementation changes the engine cannot
serialize. Arbitrary external effects have no exactly-once guarantee. Restored
tool intent without a completion record requires reconciliation or cancellation.

Cancellation is cooperative, and a synchronous provider or custom handler may
block until it returns. Hard token/cost caps are rejected because the provider
contract supplies no enforceable upper bound per operation; measured thresholds
may overshoot by the admitted operation. Missing usage stays unknown. See
[provider.md](ARCHITECTURE/provider.md) for metering details.

M4 adapters must consume these contracts. Streaming needs a transport seam for
partial output and explicit retry semantics; richer content needs a schema and
context-accounting change. Workflow and MCP adapters reuse the existing
execution, access, approval and budget boundaries. Custom tools needing several
resumable effects require a continuation protocol; synchronous callbacks cannot
be unwound and replayed for approval.

## Verification

The commands that gate a change. Each module file names the subset that proves
its own status.

```sh
cargo fmt --all -- --check                            # pass = exit 0
cargo test --workspace -q --locked                    # pass = 407 unit + 118 integration + 1 doctest
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
cargo build -p kerness --examples                     # pass = all 10 compile
cargo run -p kerness --example offline_debate         # pass = completes with no key, no network
cargo run -p kerness --example host_control           # pass = validated host result
cargo run -p kerness --example resume_approval        # pass = restored approval, each tool once
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness
cargo +1.88.0 check --workspace --all-targets --locked # pass = supported MSRV
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 502 passed
.venv/bin/python -m kerness.selfcheck                 # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                  # pass = "All checks passed!"
```

The wheel is built from `bindings/python/`, because that is where
`pyproject.toml` is:

```sh
cd bindings/python && ../../.venv/bin/maturin develop   # pass = installed workspace version
```

`.github/workflows/ci.yml` runs the format, test, lint, example-build,
`offline_debate`, self-check, rustdoc and MSRV checks. The two control/approval
examples also run locally as upgrade smoke checks. See
[testing.md](ARCHITECTURE/testing.md).

`maturin` and `python` are not on `PATH` in this workspace; invoke them from
`.venv/bin/`. `maturin` resolves the virtualenv from its own path, so it does
not matter that the venv is at the root and the command runs two levels down.

## Development Loop

Coding Discipline governs writing; Review Checks govern review. This
loop connects them and defines when work is ready to release.

```text
Frame → Write → Prove → Review → Gate
          ▲          findings      │
          └────────────────────────┘
```

### The loop

**1. Frame.** Convert the request into a goal with an observable check.
Inspect the request, code, docs, and repository conventions; record the
narrowest supported assumptions. Ask one focused question only when a
required decision cannot be discovered or safely inferred and guessing
would materially change the result. Once framed, continue without an
approval pause.

**2. Write.** Make the smallest change that reaches the goal. Add no
unrequested features or abstractions, match local style, touch only
in-scope code, and remove only orphans created by the change.

**3. Prove.** Run relevant tests and retain observable evidence.

*Survey the suite before touching it.* Before adding, changing, merging,
or deleting any test, inventory the whole suite: enumerate every test
file and case name, then read in full each test whose subject, fixtures,
or assertions touch this change. Use a subagent for broad inventory when
supported. From that inventory decide the complete set of test edits at
once — what to change, what to add, what to merge, what to remove — each
backed by `file:line`, then execute only that plan. Never write a test
before the survey, and never discover existing coverage afterward.

The plan obeys four rules:

- **Reuse or extend first.** Add a case to the test that already owns
  the behavior or shares its setup, fixtures, and subject. A new test
  function or file is justified only when the survey found no existing
  test owning the behavior, or when merging would hide which case
  failed.
- **Add only what the goal needs.** A bug fix needs a reproducing
  regression test; a new capability needs a test of its claimed
  behavior. Nothing further.
- **Retire what this change made obsolete.** Delete tests whose behavior
  no longer exists, and merge tests this change turned into duplicates,
  citing the surviving test. Leave unrelated pre-existing tests alone;
  record suspected redundancy under **Open Gaps / Roadmap**.
- **Never delete to reach green.** A failing test is a finding for
  Write. Removal requires evidence that its behavior is gone or is still
  covered elsewhere, cited by `file:line`.

Coverage of claimed behavior must not decrease. A failure returns
directly to Write, never forward to Review.

**4. Review.** Walk all seven Review Checks as separate passes. Read
whole affected files, not only the diff. Every finding needs `file:line`
evidence. Use an independent agent or isolated pass for Fit,
Dependencies, and Security when available.

**5. Gate.** Apply the Definition of Done. Any unticked criterion,
`blocker`, or unresolved `major` returns its evidence to Write. All
criteria passing means the change is ready for public or production
release. There is no separate approval or reporting phase.

### Definition of Done

**Correctness**

- The framed goal and its named check pass.
- Tests cover claimed behavior and pass; a bug fix has a regression test.
- The suite was surveyed before any test was written, changed, or
  deleted; no added test duplicates coverage another test owns, and no
  removal left claimed behavior uncovered.
- The owning module's **How to Test** command passes with evidence.
- The project builds and tests from a fresh clone without local-only
  dependencies.

**Review**

- All seven Review Checks ran; none was skipped or assumed.
- No `blocker` or unresolved `major` remains.
- Nits were applied or consciously declined.

**Legibility and contract**

- A new maintainer can build, test, run, and understand public behavior
  from the docs.
- Every changed line serves the goal; no drive-by formatting, debugging
  remnants, commented-out code, secrets, tokens, or local paths remain.
- Public names, signatures, errors, and recovery are intelligible.
- Architecture docs and `file:line` references are current.
- Breaking changes, deprecations, dependencies, licenses, and attribution
  are handled; commit or PR text explains why.

### Iterating without thrashing

- Every pass closes a named finding and touches only what it names.
- Nits alone do not trigger another pass.
- Re-run Prove after every fix.
- Two no-change passes force Gate re-evaluation: release if Done passes;
  otherwise return the surviving evidence to Frame.
- Three passes on one finding return automatically to Frame for a new
  approach.
- Never widen scope to satisfy a finding. Record out-of-scope work under
  **Open Gaps / Roadmap**.

## Coding Discipline

### 1. Think Before Coding

- Understand the request, code, goal, and repository conventions first.
- Record assumptions and choose the narrowest evidence-backed reading.
- Prefer the simpler approach when it reaches the same verified goal.
- Ask only during planning and only for a required answer that cannot be
  discovered or safely inferred.

### 2. Simplicity First

- Implement only what was requested.
- Do not add single-use abstractions, speculative flexibility, or checks
  for impossible conditions.
- If the implementation is materially larger than the problem, simplify
  it.

### 3. Surgical Changes

- Do not refactor, reformat, or clean up unrelated code.
- Match the surrounding style.
- Remove imports, variables, and functions made unused by this change;
  leave pre-existing dead code alone unless requested.
- Every changed line must trace to the stated goal.

### 4. Goal-Driven Execution

Turn work into verifiable outcomes, then loop until they pass:

- Add validation → invalid inputs are rejected by a named passing test.
- Fix a bug → a regression test fails before the fix and passes after.
- Refactor → behavior tests pass before and after.

Give every plan step its own check. Strengthen vague criteria from
repository evidence before implementation.

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

## Review Checks

Run every check against every change before merge. Keep checks separate.

Four rules bind all checks:

- **Evidence or no finding.** Every finding cites `file:line`.
- **The repository is authoritative.** Demand only conventions visible
  in the tree.
- **Read files, not only hunks.** Context can invalidate a finding or
  reveal unreachable code, unused parameters, and hidden duplication.
- **Review the change, never the author.** Describe code and impact, not
  how or by whom it was produced.

### 1. Style

Check indentation and local file conventions. Mixed indentation is
`major`; a consistent new file using the wrong local indent is `nit`.
Leave machine-checkable formatting to existing formatters and linters;
never demand unrelated reformatting.

### 2. Naming

Compare new names with nearby precedents before filing a finding. If the
repository is inconsistent, demand nothing. A local mismatch is `nit`;
an inconsistent public name is `major`.

### 3. Duplication

Search distinctive constants, errors, fields, and call sequences—not
only symbol names—for code performing the same job. Cite both sites and
the remedy. Cross-layer duplication is `major`; small local repetition
is `nit`. Similar code with meaningfully different branches is not
duplication.

### 4. Quality

Require followable control flow, errors handled where they occur, and
abstractions proportional to the problem. Swallowed errors,
inappropriate prints, unexplained magic values, and dead branches are
`major`. Remove unrequested configurability, one-caller wrappers, filler
comments, debugging remnants, and unrelated formatting. Missing tests
belong to Prove, not this check.

### 5. Fit

Read `ARCHITECTURE.md` and the owning module doc before the diff. Check
scope, layering, ownership, public-API growth, and performance claims. A
layering violation or unjustified public API is `major`. Architectural or
public-behavior changes must update the relevant docs in the same change.

### 6. Dependencies

Check manifests and imports, maintenance, supply-chain risk, advisories,
install-time behavior, license, transitive cost, and whether the standard
library is sufficient. An unjustified top-level dependency is `major`;
a live advisory or abandoned upstream is `blocker`. Incomplete evidence
does not pass.

### 7. Security

Check both defects and widened exposure: unsafe memory access, unchecked
sizes or offsets, integer overflow, path traversal, unsafe
deserialization, command construction, committed secrets, and unbounded
untrusted input. Trace input to impact; without a reachable path there is
no finding. A real defect is `major`; a trust-boundary break is `blocker`.
Describe the fix without publishing exploit steps.

### Severity and the merge threshold

| Severity | Effect |
| -------- | ------ |
| `blocker` | Must not merge. |
| `major` | Must be resolved before merge. |
| `nit` | Apply or consciously decline. |
| `info` | Context or a question; no action implied. |

Merge only with no `blocker` and no unresolved `major`. A check that did
not run does not pass. Findings feed Write and Gate directly; they do not
create a reporting phase.

## Index

- [access.md](ARCHITECTURE/access.md) — the permission boundary for commands, paths, and directories.
- [agent.md](ARCHITECTURE/agent.md) — a participant or orchestrator, and its system prompt.
- [agent-runtime.md](ARCHITECTURE/agent-runtime.md) — one agent turn: call, tool-call, feed results, repeat.
- [bindings.md](ARCHITECTURE/bindings.md) — the Rust/Python boundary and the installed package.
- [channel.md](ARCHITECTURE/channel.md) — where a session's messages are delivered.
- [compaction.md](ARCHITECTURE/compaction.md) — the context ceiling and the summarize-the-prefix rewrite.
- [context.md](ARCHITECTURE/context.md) — standing facts a source computes once per run.
- [conversation.md](ARCHITECTURE/conversation.md) — turns, transcript, and what a provider actually sees.
- [errors.md](ARCHITECTURE/errors.md) — the error enum and its Python exception hierarchy.
- [gameplan.md](ARCHITECTURE/gameplan.md) — loading a Markdown gameplan and resolving built-in assets.
- [harness.md](ARCHITECTURE/harness.md) — the frontmatter contract: parse, validate, resolve.
- [jsonschema.md](ARCHITECTURE/jsonschema.md) — strict-mode schemas and argument validation.
- [loop.md](ARCHITECTURE/loop.md) — the orchestrator loop, phases, and end reasons.
- [memory.md](ARCHITECTURE/memory.md) — what agents remember, and the store slot that keeps it.
- [persona.md](ARCHITECTURE/persona.md) — loading a persona file into prompt text.
- [prompting.md](ARCHITECTURE/prompting.md) — assembling a system prompt from its parts.
- [provider.md](ARCHITECTURE/provider.md) — talking to a model, and the four built-in backends.
- [role.md](ARCHITECTURE/role.md) — what an agent is in a session, and the chair it takes.
- [selfcheck.md](ARCHITECTURE/selfcheck.md) — `python -m kerness.selfcheck`, the installation health check.
- [session.md](ARCHITECTURE/session.md) — configuration, preparation and the blocking compatibility API.
- [run.md](ARCHITECTURE/run.md) — owned execution, scoped tools, approvals, outcomes and recovery.
- [sessionfile.md](ARCHITECTURE/sessionfile.md) — saving and resuming a run.
- [skills.md](ARCHITECTURE/skills.md) — loading skill bundles and the `Skill` tool.
- [testing.md](ARCHITECTURE/testing.md) — the three suites, the examples, CI, and release publishing.
- [toolkit.md](ARCHITECTURE/toolkit.md) — tool specs, parsing calls out of text, dispatching them.
- [toolschema.md](ARCHITECTURE/toolschema.md) — native tool dialects and their wire shapes.
- [utils.md](ARCHITECTURE/utils.md) — text scanning, retry, and Python-compatible formatting.
