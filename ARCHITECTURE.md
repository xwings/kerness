---
eatmycode_version: "1.2.0"
---

# Kerness

## Mission and Constraints

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
  and then does nothing is a bug, not a reserved word
  ([harness.md](ARCHITECTURE/harness.md)).
- **Everything is synchronous.** There is no executor, no async runtime, and no
  hidden concurrency. A session runs on the calling thread, and a stack trace
  from inside a tool handler reaches the calling `SessionRun::step` or
  `Session::run` ([run.md](ARCHITECTURE/run.md)).

### Two artifacts, one implementation

Kerness ships as two artifacts from one repository: a **Rust crate** for callers
who want the framework in a Rust program, and a **Python extension** for callers
who want to subclass `Provider`, pass a lambda as a tool handler, and hand a
`pydantic` model in for structured output. Both are supported.

**A runtime feature is written in Rust.** The crate implements it, the extension
exposes it, and the installed Python runtime surface supplies five kinds of glue:
declares a class callers subclass (`Provider`, `Channel`, `MemoryStore`),
declares one the extension cannot (the exception hierarchy, `ToolDialect` as an
`enum.Enum`, `AccessPolicy` as a dataclass), reads a signature with `inspect`,
validates with `pydantic`, or re-exports a name. A capability that exists only
in Python is a defect ([bindings.md](ARCHITECTURE/bindings.md)). The Python
[self-check](ARCHITECTURE/selfcheck.md) owns installed-package diagnostics.

Where a feature needs something only the interpreter has — `sys.stdout`, a
logger, `input`, an HTTP client under a caller's `mock.patch` — the crate names
the need as a trait (`HttpTransport`, `Logger`, `ConsoleWriter`,
`ConsolePrompt`), ships a default that works from Rust alone, and the binding
installs a replacement at `bootstrap`. The four seams, their defaults and what
the binding installs are tabulated in [design.md](ARCHITECTURE/design.md#process-wide-seams).

### Supported platforms and observable limits

| | |
| --- | --- |
| Platform | Linux and macOS; developed on Linux x86-64. Path confinement resolves every path from `/` (`crates/kerness/src/access.rs:713`), so the access boundary assumes POSIX paths; command process groups and deadlines are `#[cfg(unix)]` (`crates/kerness/src/exec.rs:14`). |
| Network | Blocking HTTP(S) to host-selected provider URLs; bundled defaults use HTTPS (`crates/kerness/src/http.rs:45`). Provider transport is outside `AccessManager`; `allowed_hosts` narrows explicit URLs in allowed commands ([access.md](ARCHITECTURE/access.md)). |
| Process | No daemon, no database, no listening socket, no background thread. |
| Filesystem | Writes are confined to paths the caller opts into: memory scopes, the session file, channel logs, and access-policy directories. |
| Runtime deps | None beyond the crate's Cargo dependencies; `pydantic` is optional and only for structured output. |

### Code-scope non-goals

These are decisions, each recorded with its reason in the owning module doc:

- No streaming; a response is one request and one reply ([provider.md](ARCHITECTURE/provider.md)).
- No parallel agent execution; the synchronous invariant is load-bearing ([run.md](ARCHITECTURE/run.md)).
- No embedded table of model context windows or prices; the caller supplies both ([provider.md](ARCHITECTURE/provider.md)).
- No per-model tokenizer; `CHARS_PER_TOKEN` plus a reactive retry ([compaction.md](ARCHITECTURE/compaction.md)).
- No MCP client, workflow adapter, or subagent scheduler yet (M4, [Roadmap](#roadmap)).
- No domain-specific bundled assets; `assets/` stays framework-generic ([gameplan.md](ARCHITECTURE/gameplan.md)).
- No hard token or cost budget; only measured thresholds ([provider.md](ARCHITECTURE/provider.md)).

### Compatibility

Compatibility is additive. Existing `ToolSpec`, `ToolHandler`, `Provider` and
`SessionSnapshot` public shapes remain usable; `Session::run` keeps result
coercion, provider-error placeholders and synchronous approval callbacks
(`RunOptions::legacy`), while `Session::start` is strict by default. Session
files are written at `SCHEMA_VERSION` 2 and valid version-1 turn boundaries
still load ([sessionfile.md](ARCHITECTURE/sessionfile.md)). The version is
declared once, as `[workspace.package] version` in the root `Cargo.toml`, and
reaches Python as `kerness.__version__` through `env!("CARGO_PKG_VERSION")`
(`bindings/python/src/funcs.rs:677`).

## Languages and Toolchain

| Area | Language | Declared support | Evidence |
| --- | --- | --- | --- |
| `crates/kerness/` | Rust, edition 2021 | MSRV **1.88**; stable toolchain | `Cargo.toml` `[workspace.package] rust-version = "1.88"`; CI `rust` job on `dtolnay/rust-toolchain@stable` |
| `bindings/python/src/` | Rust, `pyo3` 0.23 with `extension-module` + `abi3-py310` | same MSRV; `cdylib` named `_core` | `bindings/python/Cargo.toml` |
| `bindings/python/kerness/` | Python | **3.10+**, CPython, stable ABI | `pyproject.toml` `requires-python = ">=3.10"`; classifiers 3.10–3.13; CI matrix 3.10 and 3.13 |
| Build | `cargo` for the crate; `maturin` for the wheel | `maturin>=1.7,<2.0` | `pyproject.toml` `[build-system]` |
| Lint | `rustfmt` (defaults; no `rustfmt.toml`), `clippy -D warnings`, `rustdoc -D warnings`; `ruff` with `select = ["E4","E7","E9","F"]` and `target-version = "py310"` | `ruff>=0.16,<0.17` | `.github/workflows/ci.yml`; `pyproject.toml` `[tool.ruff]` |
| Test | `cargo test`; `pytest` with `pydantic` under the `dev` extra | `pytest>=7.0`, `pydantic>=2,<3` | `pyproject.toml` `[project.optional-dependencies]` |

Direct crate dependency requirements (`Cargo.toml`; resolved versions in `Cargo.lock`): `fancy-regex` 0.14, `libc`
0.2 (unix only), `regex` 1.11, `serde` 1 (derive), `serde_json` 1
(`preserve_order`), `shell-words` 1.1, `ureq` 2.12 (`json`, `tls`, no gzip),
`yaml-rust2` 0.11 (event API, no `Value` deserializer); the reasons for the
last two are comments on `[workspace.dependencies]` in `Cargo.toml`. The
binding adds `pyo3` and `serde_json` only. No Python type checker is
configured.

Locally observed (2026-09-07, not a claim of support): `cargo` 1.89 nightly
and `cargo +1.88.0`, Python 3.13.5 in `.venv`, `ruff` 0.16.5, `maturin` 1.15.0,
`pytest` 9.1.1, `pydantic` 2.13.5. `maturin` and `python` are not on `PATH` in
this workspace; invoke them from `.venv/bin/`.

The CI MSRV job pins `dtolnay/rust-toolchain@1.120.0` despite declaring a
1.88 floor (`.github/workflows/ci.yml:62`); this remains a configuration gap
([Roadmap](#roadmap)). The current `Cargo.lock` agrees with the workspace's
`0.1.2-dev` version and passes the explicit Rust 1.88 locked check.

## System Design

The Rust crate separates contract loading, provider/tool I/O, agent turns,
access checks, scheduling, persistence and session composition. `session` and
`session/run` coordinate these modules; lower-level modules do not import
`crate::session`. Peer dependencies exist: providers use tool schemas and usage
accounting, which also reference provider response types. The binding depends
on the crate; Python imports `_core`, sibling shims and interpreter support.
The dependency map, state ownership, process-wide seams and evidence-backed
decisions are in [design.md](ARCHITECTURE/design.md).

Project rules and their current evidence:

- Keep session orchestration out of lower-level modules; this is observed in
  crate imports, with no dedicated dependency-graph test.
- The crate never links Python. `crates/kerness/tests/public_api.rs:134`
  assembles a session from the public API alone.
- Runtime policy belongs in Rust; the permitted Python glue is described
  above. `bindings/python/tests/test_packaging.py:74` and `:83` check `__all__`
  declarations and export resolution, not implementation shape.
- Bundled assets exist twice, byte-identical, and nothing in the build keeps them
  in step; `bindings/python/tests/test_packaging.py:42` is the only guard.

### Trust boundaries

- **Model output is untrusted.** Tool calls are parsed defensively and validated
  against a schema before dispatch; a malformed or refused call is text returned
  to the model, never a raised error ([toolkit.md](ARCHITECTURE/toolkit.md)).
  Memory notes pass a caller `MemoryFilter` on the way in and are fenced with a
  caveat on the way out ([memory.md](ARCHITECTURE/memory.md)). Routing is a
  boundary scan for a registered name, and a role's position comes from
  frontmatter, never from prose ([role.md](ARCHITECTURE/role.md)).
- **Built-in command and file tools go through `AccessManager`.** Unlisted
  commands require approval; paths inside the workspace are allowed, with
  paths resolved before comparison. An agent workspace can only narrow the session's
  ([access.md](ARCHITECTURE/access.md)). Tool identity is assigned by the engine
  and never read from model arguments (`crates/kerness/src/session/capabilities.rs:19`).
- **The host program is trusted.** Context sources, tool handlers, providers,
  channels and stores are the caller's code; the framework meters and scopes
  them but cannot undo their effects ([run.md](ARCHITECTURE/run.md)).
- **Skill bundles are trusted only when the policy says so**
  (`trust_skill_bundles`, [skills.md](ARCHITECTURE/skills.md)).
- **Checkpoints are private files** written with `create_new` and owner-only
  permissions on Unix, then renamed into place
  ([sessionfile.md](ARCHITECTURE/sessionfile.md)); they carry prompts and tool
  arguments, so storage and retention are the host's.

### Cross-cutting invariants

- Synchronous everywhere; cancellation is a cooperative `RunControl` flag
  (`crates/kerness/src/session/run.rs:45`) checked between steps and inside
  POSIX command polling.
- Anything that touches IO returns `crate::error::Result`; the enum is flat and
  the binding fans it into one exception class per variant
  ([errors.md](ARCHITECTURE/errors.md)).
- A step dispatches at most one provider operation, one tool invocation, one
  compaction, or one maintenance scope; usage is accounted per actor and
  operation and budgets are admitted before the operation
  ([run.md](ARCHITECTURE/run.md), [provider.md](ARCHITECTURE/provider.md)).
- Intent is persisted before a tool's side effect and completion after it;
  restored intent without completion waits for reconciliation and is never
  replayed ([sessionfile.md](ARCHITECTURE/sessionfile.md)).
- Frontmatter is YAML 1.1 read as events, so `no` is a boolean and `"no"` is a
  string ([harness.md](ARCHITECTURE/harness.md)).
- Values render the way CPython renders them on both sides of the boundary
  (`crates/kerness/src/pyfmt.rs`, [utils.md](ARCHITECTURE/utils.md)).
- Resources that `'static` callbacks need live in `Arc<Shared>`
  (`crates/kerness/src/session.rs:338`) with separate resource mutexes.
  Callback paths clone handles and release locks before calling host code
  (`store_for`, `:323`).

## Runtime and Data Flow

**From Python**, `import kerness` runs `bindings/python/kerness/__init__.py`,
whose line 12 calls `_core.bootstrap(...)` with the exception classes, the
`ToolDialect` enum and the assets root; `bootstrap`
(`bindings/python/src/lib.rs:36`) installs the four seams. **From Rust**, the
caller optionally sets `kerness::assets::set_root(...)`, then `SessionConfig`
→ `Session::new` → `add_agent` / `add_tool` / `add_skill` / `add_context` →
`start` or `run`; `crates/kerness/src/lib.rs:52` onward is the re-export list.

`Session::new` (`crates/kerness/src/session.rs:549`) loads and validates the
gameplan and confines the session's own write paths. `Session::start`
(`:914`) consumes the configuration, resolves roster, defaults, permitted
tools and context, personas, skills and prompts, opens every memory scope, and
returns an owned `SessionRun`; `Session::run` (`:900`) drives the same engine
blocking. `SessionRun::step`
(`crates/kerness/src/session/run.rs:382`) applies host input, advances one
unit of work and returns `Progress`, `Waiting` or `Finished`. One agent turn is
an `AgentRunner` advancing an `AgentTurn`: assemble messages, call
`Provider::chat_with_retries` once, dispatch each tool call through
`ToolDispatcher` under `AccessManager`, feed results back, repeat until the
turn's reason is known.

Every data path with the functions it passes through, the configuration
contracts and where each is enforced, failure and recovery, the concurrency
and resource invariants, and the well-known constants both suites assert are
tabulated in [runtime.md](ARCHITECTURE/runtime.md). The short form:

- Provider errors retry with backoff; a rejected native-tools or reasoning
  body flips a one-way degrade latch ([provider.md](ARCHITECTURE/provider.md)).
- A context-overflow refusal schedules one compaction to
  `OVERFLOW_RETRY_FRACTION` and retries once ([compaction.md](ARCHITECTURE/compaction.md)).
- A failing, unknown, malformed or schema-violating tool call is answered as
  text; `MAX_INVALID_CALLS` and `MAX_REPEATED_FAILURES` end the turn
  ([agent-runtime.md](ARCHITECTURE/agent-runtime.md)).
- Strict results report `InvalidResult` diagnostics; budgets stop the next
  admitted operation with `BudgetExceeded`; a sink failure is terminal without
  replaying the completed action ([run.md](ARCHITECTURE/run.md)).
- A restored run validates counters, identities and the contract before
  executing; captured intent with no completion waits for `Reconcile` or
  cancellation ([sessionfile.md](ARCHITECTURE/sessionfile.md)).
- On supported POSIX platforms execution stays on the calling thread. The
  binding releases the GIL around `step`, contextual `run_command` and
  the HTTP transport so another Python thread can call `RunControl.cancel()`;
  every command runs in its own process group with a deadline; memory scopes
  close exactly once ([session.md](ARCHITECTURE/session.md)).

## Workspace Map

One artifact per top-level directory: `crates/kerness/` is the crate (`src/`
with 31 top-level modules plus `provider/`, `skill/` and `session/`; `assets/`;
`tests/` with 8 integration files and `common/mod.rs`; `examples/` with 10),
and `bindings/python/` is everything the wheel is made of (`pyproject.toml`,
the `kerness-py` crate in `src/` with 13 PyO3 modules, the installed package
in `kerness/`, 26 pytest modules, 8 example scripts). The root carries one
manifest, `Cargo.toml`, and one lock. `.github/workflows/` holds `ci.yml` and
`release.yml`; `.venv/`, `target/` and the caches are gitignored. The full
tree with each path's role is in [workspace.md](ARCHITECTURE/workspace.md).

Edit restrictions, each with its check on that page: the bundled assets exist
twice and both copies change together (`bindings/python/tests/test_packaging.py:42`);
the `LICENSE` and `README.md` symlinks in `bindings/python/` are load-bearing
for the wheel; `_core.abi3.so` is a built artifact to regenerate with
`maturin develop` after any Rust change (`test_packaging.py:35` checks version
agreement but cannot detect stale builds within a version); the wheel carries
the package and distribution metadata; there is no generated
source, no build script and no `.pyi` stub.

## Coding Style and Code Design

Enforced, each with its command under [Verification and Review Map](#verification-and-review-map):
`rustfmt` defaults, `clippy -D warnings` on every target, `rustdoc -D warnings`,
`rust-version = "1.88"`, `ruff` (`E4, E7, E9, F`, `py310`; `E402` ignored in
`kerness/__init__.py` because `bootstrap` must run first), package exports
(`bindings/python/tests/test_packaging.py:74`, `:83`,
`bindings/python/tests/test_selfcheck.py:18`), and the well-known constants and
request defaults agreeing across the boundary
(`crates/kerness/tests/public_api.rs:43`, `:70`; `bindings/python/tests/test_provider.py`).

Observed conventions to copy, with the sites and counts behind each in
[conventions.md](ARCHITECTURE/conventions.md):

- Every `.rs` file opens with a `//!` doc; public items carry `///` docs;
  rationale lives in those comments, never in changelog prose.
- Errors are `crate::error::{Error, Result}` wherever IO or a provider is
  touched. Process-wide lock slots use `expect("... lock poisoned")`;
  session, memory and usage mutexes recover poisoned guards. The only bare
  `.unwrap()` calls in non-test code are 13 state-machine invariants in
  `crates/kerness/src/session/run.rs`.
- `#[allow]` is local and rare; `unsafe` appears only in
  `crates/kerness/src/exec.rs` under `#[cfg(unix)]`; no `println!` or
  `eprintln!` outside the default sink in `crates/kerness/src/logging.rs`.
- Checkpoint and continuation types carry `#[serde(deny_unknown_fields)]`;
  tagged enums use `#[serde(tag = "...", rename_all = "snake_case")]`.
- Process-wide seams share one shape: a private slot accessor over a
  `OnceLock<RwLock<...>>`, a `set_*` installer, and a Rust default.
- Supplied provider behaviour lives in free functions generic over
  `P: Provider + ?Sized` so a Python override wins.
- Tests are sentences in `snake_case`; unit tests sit inline in 30 of the 42
  crate files; integration tests share `crates/kerness/tests/common/mod.rs`
  and add no test dependency.
- A Python shim is a docstring, `from kerness._core import ...`, and `__all__`;
  subclassable bases are `abc.ABC`, bundled crate types are `ABC.register`ed;
  pytest groups `class Test<Behaviour>` with `test_<sentence>` functions and
  patches the transport at `kerness.provider.http_post_json`.

Unenforced: no Python type checker runs; `auto_approve_prefixes` has no doc
comment on either surface (`crates/kerness/src/access.rs:188`,
`bindings/python/kerness/access.py:36`); the `file:line` references in these
docs are checked by review, not CI.

## Verification and Review Map

Run from the repository root. Install Rust and a Python 3.10+ development
environment, then follow the fresh-environment commands in
[verification.md](ARCHITECTURE/verification.md#local-prerequisites).

```sh
cargo fmt --all -- --check                            # pass = exit 0
cargo test --workspace -q                             # pass = 407 unit + 118 integration + 1 doctest (526)
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
cargo build -p kerness --examples                     # pass = all 10 compile
cargo run -p kerness --example offline_debate         # pass = completes with no key, no network
cargo run -p kerness --example host_control           # pass = validated host result
cargo run -p kerness --example resume_approval        # pass = restored approval, each tool once
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness   # pass = exit 0
cargo +1.88.0 check --workspace --all-targets --locked      # pass = exit 0
(cd bindings/python && ../../.venv/bin/maturin develop --extras dev) # pass = installed workspace version
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 502 passed
.venv/bin/python -m kerness.selfcheck                 # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                  # pass = "All checks passed!"
.venv/bin/python bindings/python/examples/host_control.py  # pass = validated result, exit 0
```

Verified on 2026-09-07 against the current source and lockfile: all commands
above pass with the locally observed toolchains listed above. A separate source
copy without generated artifacts also builds and passes both suites using Rust
1.88 and a fresh virtualenv. These local results do not certify the configured
CI MSRV job or untested platforms.

`.github/workflows/ci.yml` runs `rust` (fmt, clippy, test, examples,
`offline_debate`, rustdoc on stable), `msrv` (`cargo check --locked` on a
pinned toolchain that disagrees with the declared floor; see [Roadmap](#roadmap)) and `python` (3.10
and 3.13: `maturin develop --extras dev`, pytest, selfcheck, ruff).
`release.yml`'s `verify-sdist` job is the only check on the two symlinks and on
asset packaging. The job steps, the change-to-test map (which tests to run
first for each kind of change, and the review constraint the owner enforces)
and the coverage gaps are in [verification.md](ARCHITECTURE/verification.md);
[testing.md](ARCHITECTURE/testing.md) owns the suites themselves.

Coverage gaps: no test reaches the network; CI runs on Linux only;
`PyStore::revise` is the one binding crossing no test drives; nothing checks
the assets pair from the Rust side.

## Roadmap

M1 (runtime ownership and tool capabilities), M2 (host-driven execution) and
M3 (outcomes and budgets) are done in Rust and exposed through the binding;
M4 (streaming, workflow and MCP adapters, session-store listing, typed content
parts, sequential subagents) is deferred, and each part needs a separately
justified change that consumes the M1–M3 contracts. The milestone table with
its evidence, the M4 constraints, and every improvement candidate with its
owner and success check are in [roadmap.md](ARCHITECTURE/roadmap.md).

The CI `msrv` pin is an open configuration defect (`.github/workflows/ci.yml:62`).
Its proposed correction and success check are in
[roadmap.md](ARCHITECTURE/roadmap.md#repository-defects-evidence-backed-not-yet-fixed),
alongside unaccepted improvement candidates linked to their owners. The current
source and lockfile pass the declared Rust 1.88 check locally.

## Development Loop

Frame → Write → Prove → Review → Gate. Findings return to Write;
uncertainty that changes the plan returns to Frame.

Use one subagent per role when available, otherwise distinct labeled
passes. Tester and Verifier report findings and never edit; Coder repairs.

| Role | Stages | Handoff |
| ---- | ------ | ------- |
| Planner | Frame | Goal, observable checks, assumptions, affected files/owners, and plan. |
| Coder | Write | Planned changes or repairs to named findings. |
| Tester | Prove | Commands, results, and behavioral/structural evidence. |
| Verifier | Review + Gate | Evidence-backed findings or verified completion. |

### The loop

1. **Frame:** Inspect the request, code, docs, and conventions before
   planning. Give the goal and each plan step an observable check. When
   using eatmycode, run its Version and Freshness Gate before trusting
   architecture; include versions, migration scope, Index/agent-file
   changes, and verification commands in architecture plans. Resolve
   uncertainty from evidence and record the narrowest supported assumptions.
   Only Planner may ask one focused question, when a required decision
   cannot be discovered or safely inferred and guessing changes the result.
2. **Write:** Apply Coding Discipline. Make the planned change; for a
   repair, address only named findings. Update affected architecture with
   changes to its documented contracts.
3. **Prove:** Run relevant tests and structural checks, retaining observable
   evidence. For architecture work under eatmycode, apply its Architecture
   Verification. Failures and missing, duplicate, or obsolete coverage
   become Coder findings. Re-run affected checks after repairs; never send
   a red result to Review.
4. **Review:** Apply every Review Check as a separate pass over full affected
   files. Use an independent agent or isolated pass for Fit, Dependencies,
   and Security when available. Return findings to Coder, then re-prove
   and re-review the repairs.
5. **Gate:** Confirm completion only when the Definition of Done passes.
   Return unmet criteria to the responsible stage; continue until resolved.
   If an external constraint prevents verification, state the missing
   evidence and remaining work without claiming completion or readiness.

Handoffs are automatic. Continue without pauses for plan approval,
permission to continue, or review/reporting ceremonies. Finish with the
harness's normal concise completion handoff.

### Definition of Done

- **Correctness:** The goal and named checks pass. Tests cover claimed
  behavior; bug fixes have a reproducing regression test. The project
  builds and tests from a fresh clone without local-only dependencies.
  Owning modules' **How to Test** commands pass with evidence.
- **Review:** Every Review Check ran and its completion threshold passes.
- **Contract:** Docs reflect source and let an agent locate owners,
  constraints, and verification commands. When using eatmycode, architecture
  satisfies its Output Contract, verification, and version rules. Public
  names, signatures, errors, and recovery are intelligible. Breaking
  changes, deprecations, dependencies, licenses, and attribution are handled;
  commit or PR text, when present, explains why.
- **Scope:** Changed lines serve the goal and follow Coding Discipline;
  no debugging remnants, commented-out code, secrets, tokens, or local paths
  remain. Test edits follow the inventory and coverage rules below.

### Iterating without thrashing

- Each repair pass targets a named finding; nits alone do not trigger one.
- Two no-change passes force Gate re-evaluation. If Done still fails,
  return the surviving evidence to Frame.
- Three passes against the same finding return to Frame for a new approach.
- Never widen scope to satisfy a finding. Record coding follow-ups under
  **Open Gaps / Roadmap** and keep non-coding work outside architecture.

## Coding Discipline

- Implement only the goal. Prefer the simplest approach that passes its
  checks; simplify code materially larger than the problem.
- Match local style. Avoid speculative features, flexibility, single-use
  abstractions, and checks for impossible conditions.
- Keep edits surgical: no unrelated refactoring, reformatting, or cleanup.
  Remove imports, variables, and functions made unused by this change;
  leave pre-existing dead code alone unless requested.
- Make success concrete: validation rejects invalid input in a named test;
  a regression test fails before a bug fix and passes after; behavior tests
  pass before and after a refactor.

### Before editing tests

Before any test edit, including during Write, inventory the whole suite:
enumerate every test file and case name, then read in full tests whose
subject, fixtures, or assertions touch the change. Use a subagent for broad
inventory when supported. Plan all additions, changes, merges, and removals
from that evidence, citing `file:line`, before executing the test edits.

- **Reuse first:** Extend the test owning the behavior or sharing its
  setup, fixtures, and subject. Add a function/file only if no existing
  owner fits or merging would obscure which case failed.
- **Add only required coverage:** A bug fix needs its regression test;
  a capability needs a test of its claimed behavior. Avoid duplicates.
- **Retire only what changed:** Remove tests of deleted behavior and merge
  new duplicates, citing surviving coverage. Record unrelated suspected
  redundancy under **Open Gaps / Roadmap**.
- **Preserve coverage:** Never delete or weaken tests to turn red green.
  Removal needs evidence that behavior is gone or covered elsewhere;
  coverage of claimed behavior must not decrease.

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

Run every check against every change before confirming a code edit is
complete, even when no commit or merge is requested. Keep checks separate.

- **Evidence or no finding:** Cite `file:line` for every finding.
- **Repository authority:** Demand only conventions supported by the tree.
- **Full context:** Read affected files, not only hunks; context can expose
  unreachable code, unused parameters, or hidden duplication.
- **Code and impact:** Review the change, never the author or how it was made.

### 1. Style and Naming

Check indentation and local conventions; leave machine-checkable formatting
to existing formatters/linters and never demand unrelated reformatting.
Mixed indentation is `major`; a consistent new file with the wrong local
indent is `nit`. Compare names with nearby precedents. If the repository
is inconsistent, demand nothing. A local naming mismatch is `nit`; an
inconsistent public name is `major`.

### 2. Duplication

Search distinctive constants, errors, fields, and call sequences, beyond
symbol names, for the same job. Cite both sites and a remedy. Cross-layer
duplication is `major`; small local repetition is `nit`. Similar code with
meaningfully different branches is not duplication.

### 3. Quality

Require followable control flow, errors handled where they occur, and
proportionate abstractions. Swallowed errors, inappropriate prints,
unexplained magic values, and dead branches are `major`. Remove unrequested
configurability, one-caller wrappers, filler comments, debugging remnants,
and unrelated formatting. Missing tests belong to Prove.

### 4. Fit

Read the root architecture and owning module before the diff. Check
language/toolchain constraints, conventions, scope, layering, ownership,
invariants, public-API growth, compatibility, and performance claims against
source. A layering violation or unjustified public API is `major`.
Architectural/public-behavior changes need matching docs in the same change.

### 5. Dependencies

Check manifests/imports, maintenance, supply-chain risk, advisories,
install-time behavior, license, transitive cost, and standard-library
alternatives. An unjustified top-level dependency is `major`; a live
advisory or abandoned upstream is `blocker`. Incomplete evidence does not pass.

### 6. Security

Check defects and widened exposure: unsafe memory access, unchecked sizes
or offsets, integer overflow, traversal, unsafe deserialization, command
construction, committed secrets, and unbounded untrusted input. Trace input
to impact; without a reachable path there is no finding. A real defect is
`major`; a trust-boundary break is `blocker`. Describe fixes without exploit
steps.

### Severity and the completion threshold

| Severity | Effect |
| -------- | ------ |
| `blocker` | Must not confirm completion or merge. |
| `major` | Must be resolved before confirming completion or merging. |
| `nit` | Apply or consciously decline. |
| `info` | Context or a question; no action implied. |

Confirm completion or merge only with no `blocker` or unresolved `major`.
A check that did not run does not pass; explain evidence-backed
inapplicability. Findings feed Write and Gate directly.

## Index

Run the freshness gate and read the root's cross-cutting rules, then use the
[Module index](ARCHITECTURE/index.md#owners) to select the owning subsystem by
source path or change trigger. Its 27 entries list responsibilities and
integration partners; read the selected owner, relevant partners, and cited
source and tests. The [supporting-page index](ARCHITECTURE/index.md#supporting-pages)
routes expanded root guidance and memory store implementation detail.
