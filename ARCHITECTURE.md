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
| Platform | Linux and macOS; developed on Linux x86-64. `ureq` + `rustls` reach further, but path confinement resolves every path from `/` — `crates/kerness/src/access.rs:473` — so the access boundary assumes POSIX paths |
| Network | Outbound HTTPS only, to provider endpoints the caller names |
| Runtime deps | None beyond the crate's Cargo dependencies; `pydantic` is optional and only for structured output |

There is no daemon, no database, no listening socket, and no background thread.
Filesystem writes are confined to paths the caller opts into: the memory file,
the session file, channel logs, and directories added to the access policy.

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
    src/                    29 top-level modules plus provider/ and skill/,
                            documented by subsystem in the Index
    assets/                 built-in gameplans, roles, personas, skills
    tests/                  101 integration tests over the crate's public surface
    examples/               8 harnesses driven from Rust alone
bindings/
  python/                   everything the wheel is built from
    pyproject.toml          the wheel's manifest; `pip install .` runs here
    Cargo.toml              the `kerness-py` crate, a workspace member
    LICENSE  README.md      symlinks to the root copies
    src/                    11 modules, one per boundary concern
    kerness/                the installed Python package
      __init__.py           bootstrap + public surface
      *.py                  per-subsystem re-export shims
      provider.py           deliberate Python (see ARCHITECTURE/provider.md)
      access.py             deliberate Python (console prompt)
      selfcheck.py          deliberate Python (import health)
      gameplans/ roles/ personas/ skills/   the same assets, installed
    tests/                  26 pytest modules over the Python surface
    examples/               runnable harnesses, walked by test_examples.py above
.github/workflows/          CI on every push; release builds wheels and an sdist
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
   the exception classes (two-argument constructors a `create_exception!` cannot
   express), the `ToolDialect` enum (callers compare members with `is`, so it
   must be a real `enum.Enum`), and the assets root (only the package knows where
   pip put it). `bootstrap` also installs the HTTP transport seam and the logger
   — `bindings/python/src/lib.rs:34`.
3. The remaining imports pull the public names out of the per-subsystem shims.
4. The caller builds a `Session(...)`, registers agents, tools, and skills,
   and calls `run()`.

### From Rust

1. The caller sets `kerness::assets::set_root(...)` if the built-in gameplans are
   wanted from outside the crate directory, otherwise `$KERNESS_ASSETS` or
   `$CARGO_MANIFEST_DIR/assets` resolves it — `crates/kerness/src/assets.rs:38`.
2. `SessionConfig { .. }` → `Session::new` → `add_agent` → `run`.

### Inside `run()`

`Session::run` (`crates/kerness/src/session.rs:646`) is the whole harness:
it resolves the gameplan's harness spec against what was registered, builds the
skill registry and access manager, seeds the `Conversation`, then either runs the
round-robin participant loop or hands control to `OrchestratorLoop::run`
(`crates/kerness/src/orchestrator.rs:462`) when the gameplan declares an
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
| `DEFAULT_ROLE_FILE` | `participant.md` | `crates/kerness/src/role.rs:66` |
| `DEFAULT_TERMINATORS` | `CONSENSUS_REACHED`, `END_SESSION` | `crates/kerness/src/utils.rs:12` |
| `RESERVED_TOOL_NAMES` | `["Skill"]` | `crates/kerness/src/harness.rs:25` |
| `DEFAULT_TIMEOUT` | 60s | `crates/kerness/src/exec.rs:18` |
| `ReasoningEffort::default()` | `high` | `crates/kerness/src/provider/mod.rs:63` |
| `DEFAULT_REQUEST_TIMEOUT_SEC` | `60` | `crates/kerness/src/provider/mod.rs:39` |
| `DEFAULT_RETRIES` | `2` | `crates/kerness/src/provider/mod.rs:41` |
| `DEFAULT_BACKOFF_SEC` | `2.0` | `crates/kerness/src/provider/mod.rs:43` |
| `DEFAULT_TEMPERATURE` | `1.0` | `crates/kerness/src/provider/mod.rs:45` |
| `DEFAULT_TOP_P` | `1.0` | `crates/kerness/src/provider/mod.rs:47` |
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

Framework work that is not built yet. Everything else this document describes
exists and is tested; the [Verification](#verification) gate runs green at the
end of each of these, not only at the end of the list.

| Planned | |
| --- | --- |
| **Runtime events and a step machine** | A typed synchronous `EventSink` threaded through session, loop, and agent runtime, `Session::step()` so a caller drives the run rather than blocking on `run()`, cancellation between steps, approval as a resumable event rather than a console read, and a defaulted `Provider::chat_streaming` each backend opts into. `Channel` becomes an adapter over the sink. The event protocol and the step machine are one change: an approval-requested event is meaningless if the caller cannot answer it. |
| **Content parts, a session store, and lifecycle hooks** | `Message.content` becomes typed parts — text, images, structured tool output — which moves `SCHEMA_VERSION` to `2` with a migration, and changes what `CHARS_PER_TOKEN` can claim. A `SessionStore` adds IDs, listing, forking, and replay over today's single-file snapshot. Hooks arrive as an `EventSink` whose handlers can veto, rather than as a second interception mechanism beside the first. |
| **MCP, subagents, budgets, and bundle-defined tools** | An MCP client as an adapter into the existing tool registry, stdio first because it is synchronous. A sequential subagent primitive — parallel fan-out would be the first breach of *no hidden concurrency* and would need that invariant amended deliberately. Usage and cost aggregated into enforceable budgets that stop a run with a named end reason. And a skill directory that defines its own tools over `run_command`, gated on `trust_skill_bundles`, so a skill ships capability rather than only prose. |

## Verification

The commands that gate a change. Each module file names the subset that proves
its own status.

```sh
cargo fmt --all -- --check                            # pass = exit 0
cargo test --workspace -q                             # pass = 342 unit + 101 integration, 0 failed
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
cargo build -p kerness --examples                     # pass = all 8 compile
cargo run -p kerness --example offline_debate         # pass = completes with no key, no network
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 443 passed
.venv/bin/python -m kerness.selfcheck                 # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                  # pass = "All checks passed!"
```

The wheel is built from `bindings/python/`, because that is where
`pyproject.toml` is:

```sh
cd bindings/python && ../../.venv/bin/maturin develop   # pass = "Installed kerness-0.1.0"
```

`.github/workflows/ci.yml` runs all of the above on every push, plus
`cargo doc --no-deps -p kerness` under `RUSTDOCFLAGS=-D warnings` and
`cargo check --workspace --all-targets --locked` on the MSRV toolchain. See
[testing.md](ARCHITECTURE/testing.md).

`maturin` and `python` are not on `PATH` in this workspace; invoke them from
`.venv/bin/`. `maturin` resolves the virtualenv from its own path, so it does
not matter that the venv is at the root and the command runs two levels down.

## Development Loop

Coding Discipline governs how code is written. Review Checks govern how it
is reviewed. This is the loop that runs them, and the gate that ends it.

Code that runs is not code that is done. Done is defined below, it is
checked rather than felt, and the only way to reach it is to go round.

### The loop

```
      ┌──────────────────────────────────────────────────────┐
      │                    findings remain                   │
      ▼                                                      │
  1. FRAME  →  2. WRITE  →  3. PROVE  →  4. REVIEW  →  5. GATE
     think       build        evidence     7 checks     done?
                                                          │
                                                          ▼
                                                        ship
```

**1. Frame — think before touching code.** Restate the task as a goal
with a check attached: not "add validation" but "invalid input is
rejected with a named error, proven by a test that fails today". State
your assumptions out loud. If the task has more than one reading, present
them rather than picking silently. If a simpler approach exists, say so
before building the complicated one. If something is unclear, stop and
ask — an hour of building on a wrong assumption costs more than the
question. For multi-step work, write the plan as steps with a check on
each. Detail: Coding Discipline §1 and §4.

**2. Write — the smallest change that reaches the goal.** No features
beyond the ask, no abstraction for a single caller, no configurability
nobody requested, no error handling for conditions that cannot occur.
Touch only what the goal requires; match the surrounding style even
where you would do it differently; clean up the orphans your own change
created and nothing else. Detail: Coding Discipline §2 and §3.

**3. Prove — produce evidence, not belief.** Run the module's **How to
Test** command and keep the output. A bug fix carries a test that failed
before the change and passes after; a new capability carries a test of
the behaviour it claims. "It should work" is not a result, and neither
is a test you wrote but did not run. If Prove fails, return to Write —
do not carry a red test into Review.

**4. Review — walk all seven checks against your own diff.** Style,
Naming, Duplication, Quality, Fit, Dependencies, Security, in order, one
pass each. Change stance rather than merely re-reading: read the diff as
someone looking for a reason to reject it. Read whole files for
Duplication and Quality — the hunk hides unused parameters, unreachable
branches, and forward-only wrappers. The evidence rule binds against
yourself: a self-review finding without `file:line` is a mood, not a
finding. Where an independent reviewer is available — another agent,
another pass, another person — send Fit, Dependencies, and Security
there; those are the judgements authorship blinds you to hardest.

**5. Gate — check Done, then decide.** Walk the Definition of Done. Every
box ticked: ship, and report the checklist. Any box unticked: name the
findings that block it and return to Write with those findings and
nothing else. Record which box failed — an unexplained extra pass is
indistinguishable from thrashing.

### Definition of Done

The bar is open-source standard: a maintainer who has never seen this
change, and cannot ask you anything, could review and merge it from the
diff and the docs alone. Concretely, all of:

**Correctness**

- The goal stated in Frame is met, and the check named in Frame passes.
- Tests cover the behaviour the change claims and they pass. A bug fix
  has a test that fails without it.
- The owning module's **How to Test** command passes, output recorded.
- It builds and tests clean from a fresh clone — no uncommitted file it
  depends on, no step that exists only on your machine.

**Review**

- All seven Review Checks walked, each with a recorded status. None
  skipped, none assumed.
- No `blocker`. No unresolved `major`.
- Nits applied or consciously declined.

**Legibility — this is the part that makes it open source**

- A stranger can build, test, and run it from the README alone.
- Public names, signatures, and errors are intelligible without reading
  the implementation. Error messages tell the reader what to do next.
- Every changed line traces to the stated goal. No drive-by
  reformatting, no debugging leftovers, no commented-out blocks, no
  secrets, tokens, or absolute local paths.
- The commit or PR message says *why*. The diff already says what.

**Contract**

- The architecture docs are current: Review Check 5 passes, and existing
  `file:line` references still resolve.
- Breaking changes are called out and justified; deprecations documented.
- Anything vendored, copied, or newly depended upon is
  license-compatible and attributed.

"Good enough for me" is not on this list, and neither is "I feel good
about it" — Coding Discipline §4 rejects exactly that: strong criteria
let the loop run unattended, weak criteria stall it on your mood. When a
box cannot be ticked, the loop is not finished. Say which box and why.

### Iterating without thrashing

The loop has to stop. These rules end it in both directions — too early
and never.

- **Every pass closes a named finding.** "Have another look" is not a
  pass. If you cannot name what this pass fixes, do not start it.
- **A pass touches only what its findings name.** A review finding
  authorises repair, not redesign. Surgical Changes still binds inside
  the loop, and a review-driven rewrite is the most expensive way to
  violate it.
- **Nits alone do not justify a pass.** Batch them into a pass that
  something real triggered, or decline them and say so.
- **Re-run Prove after every pass.** A fix that was not re-tested is not
  a fix, and passes two onward are where regressions enter.
- **Two consecutive passes that change nothing: stop.** Either it has
  converged — ship — or you are stuck. Say which.
- **Three passes against the same finding: return to Frame.** The design
  is wrong, not the code. Iterating on the implementation will not fix
  it.
- **Never widen scope to satisfy a finding.** Work the finding demands
  beyond the stated goal is a follow-up: record it under the module's
  **Open Gaps / Roadmap**, say you deferred it, and leave it there.

**The loop is working if:** defects are found by Review rather than by
users, passes shrink as they go, and Gate is a check you perform rather
than a feeling you have.

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

## Review Checks

Run these seven checks against every change before proposing it for
merge. Each is a separate pass with its own scope; a pass that blends
into another performs neither. Four rules bind all seven:

- **Evidence or no finding.** Every finding cites `file:line`.
  "Inconsistent naming", "this seems unsafe", and "consider refactoring"
  with nothing to point at are not findings and are not filed.
- **The repository is the authority.** A convention nobody can locate in
  the tree is not a convention, and nothing may be demanded on its
  authority. Where a general rule and this repository's actual practice
  disagree, the repository wins.
- **Read the file, not the hunk.** Unused parameters, unreachable
  branches, forward-only wrappers, and a validation check one frame up
  are all invisible in a diff.
- **Review the change, never the author.** Say what the code does. Do not
  assert how it was produced and do not speculate about who wrote it.

### 1. Style

Indentation and formatting, per language and per file. Mixed indentation
inside one file is **major** — it breaks anyone whose editor is set for
the other one. A new file that is internally consistent but uses the
wrong indent is a **nit**. Reindenting lines the change did not otherwise
need to touch is one finding asking for it to be split out, not one
finding per line.

Line length, import order, trailing whitespace, and end-of-line markers
belong to the formatter and linter in CI. Do not spend attention on what
a machine already handles.

### 2. Naming

Whether new names follow this project's conventions — variables,
functions, methods, classes, modules, files, constants, parameters.

Conventions are discovered, never assumed: before calling a name wrong,
find the closest existing names and read how they are spelled. Cite two
or three, then propose the specific replacement. Where the repository is
genuinely inconsistent in an area, say so and demand nothing. A name
defensible in general but foreign here is still a finding; a name ugly in
general but matching ten neighbours is not. **Nit** inside a module,
**major** on a public name — the project lives with public names far
longer than with the change that introduced them.

### 3. Duplication

Whether the change adds something the project already has.

Near-equivalent means same job, not same text: a helper that resolves a
path, parses a structure, or wraps a call the same way under a different
name counts, and copy-pasted blocks across two platform or backend
implementations count loudest. Grep the distinctive strings inside the
new code — a constant, an error message, a field name, an unusual call
sequence — not only the symbol names; the expensive duplicates are the
ones that do not share a name.

Every finding cites both sites and names the remedy: call the existing
helper, merge the two, or lift the shared part out — and say where the
shared code should live. Two functions that look alike and differ in one
branch are not duplicates. Duplication spanning layers is **major**;
small repetition inside one new module is a **nit**.

### 4. Quality

Whether the code is well written, and whether anything is in the change
that should not be there.

Well written means control flow is followable, errors are handled where
they occur, the abstraction matches the problem, and a reader can tell
what the code does without running it. Exception handlers that swallow
without re-raising or logging, prints where the project has a logger,
unnamed magic numbers, and dead branches are **major**.

Extra weight is the half that reviewers skip: code the change does not
need, configurability nobody asked for, abstractions with exactly one
caller, commented-out blocks, debugging leftovers, comments that restate
the line beneath them, docstrings that name every parameter and explain
none, defensive checks for conditions that cannot occur, and unrelated
reformatting smuggled in beside the real change. Say plainly that it can
be deleted and what breaks if it is not. Filler comments and unused
parameters are **nits**. Missing tests are an observation here, not a
finding — CI owns correctness.

### 5. Fit

Whether the change belongs in this project and leaves it better rather
than merely larger.

Read `ARCHITECTURE.md` and the owning `ARCHITECTURE/<module>.md` before
the diff — that ordering is the point of the doc set. Then ask: is this
in scope, or is it something to build on top of the project instead of
inside it? Does it respect the layering, or reach across a boundary the
architecture keeps closed? Does it deliver a fix, a real capability, or a
measured speedup — or add surface, cost, and maintenance burden for a
gain nobody has stated?

Test performance claims for plausibility: a change that claims to be
faster while adding a per-call allocation to a hot path is making things
worse. A new public entry point overlapping one that exists grows the API
the project must support forever. Layering violations and unjustified
public-API growth are **major**. Where the goal is sound but the
placement is wrong, say where it should go.

A change that alters architecture, ownership, data flow, integration
points, or public behaviour without updating `ARCHITECTURE.md` and the
relevant `ARCHITECTURE/<module>.md` in the same change fails this check.

### 6. Dependencies

Whether the change adds a package, and whether it should.

Four questions, in order. Is one actually being added — check the
manifests *and* the imports, because code can import what the manifest
never declared. Is it maintained: last release, cadence, maintainer
count, archived upstream? Is it a supply-chain risk: known advisories, a
very recent first release, a single maintainer holding a
widely-installed name, a name close to a popular one, install-time code
execution? And is it worth it — the honest comparison is the package
plus its transitive tree plus its future breakage, against the
standard-library code it would replace.

The default answer to a new dependency is no and the burden is on the
change to move it, but say so proportionately: a well-maintained package
doing something genuinely hard is a good trade, and pretending otherwise
is not rigour. An unjustified new top-level dependency is **major**; a
live advisory or an abandoned upstream is a **blocker**. When the
evidence cannot be gathered, say the check could not be completed rather
than guessing.

### 7. Security

Whether the change introduces a security defect, and whether it widens
exposure. Two distinct questions.

Defects in the change: memory safety in native code, unchecked lengths
and offsets, integer overflow feeding an allocation or an index, path
traversal, unsafe deserialisation, commands built from input the code
does not control, secrets or tokens committed, untrusted input reaching a
parser without bounds.

Exposure, separately: whatever this project treats as untrusted — user
input, network data, sandboxed or emulated code, third-party content — a
change that lets it reach the host filesystem, a subprocess, the network,
or host memory is a serious finding even when the change contains no bug
of its own.

State the path from input to impact concretely: where the value enters,
what it is not checked against, and what an attacker gets. If you cannot
trace that path you have a question, not a finding — ask it, and withdraw
it once answered. No severity inflation: a theoretical issue with no
reachable path is **info**, a real bug in the change is **major**, and
anything that breaks a trust boundary is a **blocker**. Describe the flaw
and the fix; do not publish exploit steps.

### Severity and the merge threshold

| Severity | Meaning | Effect |
| -------- | ------- | ------ |
| `blocker` | Breaks a trust boundary, corrupts data, or ships a known-vulnerable dependency. | Must not merge. |
| `major` | Wrong, unsafe, or costly enough that a maintainer would ask for a change. | Merge only with a stated, accepted reason. |
| `nit` | Correct, but inconsistent with the project. | Author's call. |
| `info` | Observation, question, or context. | No action implied. |

Merge only when no `blocker` and no unresolved `major` remains. A verdict
of "looks good" alongside a `major` finding is not an approval — resolve
the finding or record why it stands.

### Reporting

Report once, as one document, not seven. Open with what the change is
trying to do and what it gets right. List the findings that survived a
verify pass, ordered by severity, each with its `file:line` and the
reason it matters. Close with what would have to change. Then record the
checklist, so it is visible which checks were actually walked:

| Check | Status | Note |
| ----- | ------ | ---- |
| 1. Style | pass / concern / blocker | evidence, or "nothing found" |
| 2. Naming | | |
| 3. Duplication | | |
| 4. Quality | | |
| 5. Fit | | |
| 6. Dependencies | | |
| 7. Security | | |

A check nobody could complete is a **concern**, not a pass. Never report
a check that did not run: a confident verdict covering skipped checks
reads exactly like a real one, which makes it worse than no review.

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
- [role.md](ARCHITECTURE/role.md) — what an agent is in a session, and the chair it takes.
- [selfcheck.md](ARCHITECTURE/selfcheck.md) — `python -m kerness.selfcheck`, the installation health check.
- [session.md](ARCHITECTURE/session.md) — the top-level object that assembles and runs everything.
- [sessionfile.md](ARCHITECTURE/sessionfile.md) — saving and resuming a run.
- [skills.md](ARCHITECTURE/skills.md) — loading skill bundles and the `Skill` tool.
- [testing.md](ARCHITECTURE/testing.md) — the three suites, the examples, and what CI runs.
- [toolkit.md](ARCHITECTURE/toolkit.md) — tool specs, parsing calls out of text, dispatching them.
- [toolschema.md](ARCHITECTURE/toolschema.md) — native tool dialects and their wire shapes.
- [utils.md](ARCHITECTURE/utils.md) — text scanning, retry, and Python-compatible formatting.
