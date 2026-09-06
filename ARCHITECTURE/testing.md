---
eatmycode_version: "1.1.0"
---

# Testing

## Goal

Three suites, each proving something the other two cannot, and a CI that runs
all of them on every push. This module owns the test doubles, the scratch
directories, the suite boundaries, and the commands that gate a change. It does
not own any behaviour under test; each subsystem doc maps its own invariants to
the tests that hold them.

The framework ships as two artifacts, so it can fail in three distinct ways: the
logic can be wrong, the Rust surface can be unusable, or the boundary to Python
can be broken. A suite that catches one of those says nothing about the others.

| Suite | Where | What only it can catch |
| --- | --- | --- |
| Unit | `#[cfg(test)]` inside `crates/kerness/src/` | A function that computes the wrong answer. Reaches internals a caller cannot. |
| Integration | `crates/kerness/tests/` | A session that cannot be *assembled* — a missing re-export, a type a dependent cannot name, a run whose parts are each right and whose whole is not. |
| Python | `bindings/python/tests/` | Anything about the boundary: a value that crosses wrong, a subclass the extension will not accept, an asset the wheel did not install. |

## Status

`done` — `cargo test --workspace -q` passes 407 unit tests, 118 integration
tests and one doctest; `.venv/bin/python -m pytest bindings/python/tests -q`
passes 502; format, clippy, rustdoc, the example build, the three offline
examples, self-check and ruff all exit 0. The two `--locked` commands in the
gate fail against the committed `Cargo.lock` — see **Open Gaps / Roadmap**.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/tests/common/mod.rs` | the doubles all eight integration files share |
| `crates/kerness/tests/*.rs` | one file per behaviour cluster, 118 tests |
| `crates/kerness/src/testing.rs` | the unit suite's `TempDir`, behind `#[cfg(test)]` |
| `bindings/python/tests/conftest.py` | the Python suite's doubles and fixtures |
| `bindings/python/tests/test_*.py` | 26 modules, 502 tests |
| `bindings/python/tests/test_examples.py` | walks `bindings/python/examples/` by AST and asserts every name it reaches for exists |
| `crates/kerness/examples/*.rs` | 10 examples, compiled by CI; `support/mod.rs` holds the offline fixtures the two host-control examples share |
| `.github/workflows/ci.yml` | what runs on push and pull request |
| `.github/workflows/release.yml` | wheels, sdist, and the clean-interpreter install check |

## Language and Conventions

Rust integration tests and inline unit tests, plus pytest; the root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- Test names are sentences in snake_case in both languages
  (`a_session_assembles_from_the_public_api_alone`,
  `test_a_bare_policy_allows_nothing`); Python groups cases in `Test<Behaviour>`
  classes. Observed, not enforced.
- `crates/kerness/tests/common/mod.rs:15` carries `#![allow(dead_code)]`: each
  test binary compiles the module and uses part of it.
- Provider doubles are built on `ProviderBase::new(0, 0.0, None)`
  (`crates/kerness/tests/common/mod.rs:113`): zero extra attempts, so one
  scripted reply means exactly one call and a failure surfaces instead of being
  slept over. `conftest.py` does the same with `retries=0, backoff_sec=0`
  (`bindings/python/tests/conftest.py:9`).
- Scratch directories are hand-rolled, not `tempfile`: the crate's claim is that
  it needs nothing beyond what `Cargo.toml` lists, and that holds for the test
  build as well. `crates/kerness/src/testing.rs:16` is the unit-suite copy, used
  by eleven modules; `crates/kerness/tests/common/mod.rs:422` is the
  integration copy, because an integration test is a separate crate and cannot
  see anything behind `cfg(test)`. Two copies is the floor the crate boundary
  sets.
- Python: `ruff` with `select = ["E4", "E7", "E9", "F"]` from
  `bindings/python/pyproject.toml`; pytest is configured by no file (observed);
  no type checker is configured.
- The Python column is a boundary, not a second opinion. A Python test earns its
  place when the binding does work — a callable crossing as a handler, a
  subclass answering by Python method resolution, an exception arriving as the
  right class, a constant spelled in both languages. Restating a crate rule
  through a pass-through binding cannot fail unless the crate test fails first,
  and two suites asserting one rule drift apart. Where a value is plain data the
  crate validates — `allowed_hosts`, an agent's `tools` — one case proves it
  crossed, and the semantics are asserted where they are decided.

## Design and Invariants

**The integration suite compiles as a dependent does.** Everything in
`crates/kerness/tests/` reaches the crate only through `kerness::…`
(`crates/kerness/tests/public_api.rs:1`), so a `pub` item that only the unit
tests exercise can be removed without a test failing — and the break lands on
whoever depended on it. `public_api.rs` exists to make that break land here.

**Process-wide seams are shared by concurrent tests.** The four seams in the
root's table are `OnceLock<RwLock<Arc<dyn Trait>>>` slots, and the unit suite
runs concurrently in one process, so a test that installs a double shares that
slot with whatever else is running. Two answers, picked by what the test needs to
read back:

- A test that only checks its double was reached tolerates a neighbour writing
  through the same slot, and asserts with `contains` rather than equality —
  `crates/kerness/src/channel.rs:440`.
- A test that reads back the exact question *its own* double recorded cannot,
  because a neighbour installing over the slot mid-body takes that recording
  away; those installs take turns on a static mutex —
  `crates/kerness/src/access.rs:1125`.

Neither is optional. Skipping the second is not a flaky test but a race, and it
fails as an index out of bounds on an empty recording, which names the assertion
rather than the seam.

**Nothing reaches the network.** Every provider in every suite is a double; the
four backends are proved down to the request they build and the response they
parse. `offline_debate` drives a real gameplan to completion the same way, which
is why CI runs it as the smoke test (`.github/workflows/ci.yml:39` onward).

**Examples are compiled, not read.** `cargo build -p kerness --examples` fails
on an example that no longer matches the API. The Python examples need keys and
cannot run, so `bindings/python/tests/test_examples.py:132` parses each one and
asserts every `kerness.X` attribute, `Session` method and `SessionResult`
attribute it names still exists; `:148` loads every gameplan shipped beside an
example under the current schema.

**Two asset copies, one guard.** `crates/kerness/assets/` and
`bindings/python/kerness/{gameplans,roles,personas,skills}/` must stay
byte-identical, and nothing in the build keeps them so;
`bindings/python/tests/test_packaging.py:42` is the only check, and it needs the
Python surface installed.

**The sdist installs from clean.** `release.yml`'s `verify-sdist` job
(`.github/workflows/release.yml:87`) installs the source distribution into an
interpreter with no checkout beside it (`:92`) and runs `kerness.selfcheck`, so
an asset the package failed to include cannot be masked by the working tree. It
is also the only check on the `LICENSE` and `README.md` symlinks in
`bindings/python/`: `license-files` and `readme` resolve against that directory,
and if they resolve to nothing the build succeeds and ships less.

## Key Types and Entry Points

- `crates/kerness/tests/common/mod.rs:36` — `Call` — one request a double
  received. `system()`, `text()` and `last()` are the three questions tests ask
  of it; `purpose` is how a test tells an orchestrator turn from a participant's.
- `crates/kerness/tests/common/mod.rs:97` — `ScriptedProvider` — replies written
  in advance, keyed by purpose substring in declaration order, each key owning a
  sequence with its own cursor and a last entry that repeats.
- `crates/kerness/tests/common/mod.rs:272` — `ToolProvider` — emits native tool
  calls under a chosen `ToolDialect`, which is how the OpenAI and Anthropic wire
  shapes are exercised without a network.
- `crates/kerness/tests/common/mod.rs:366` — `RecordingChannel` — what was
  delivered, as against what the transcript holds. The two differ, and the
  difference is a tested behaviour.
- `crates/kerness/tests/common/mod.rs:422` — `TempDir` —
  `env::temp_dir()/kerness-test-{pid}-{tag}-{counter}`, removed on `Drop`.
- `crates/kerness/tests/common/mod.rs:500` — `refusal<T>(Result<T>) -> String` —
  `Session` does not implement `Debug`, so `expect_err` is unusable; this is how
  a test reads a rejection.
- `crates/kerness/tests/common/mod.rs:515` — `confine(settings, temp)` — sets
  the workspace and memory path into the scratch directory; an unset workspace
  is the process's current directory, which a temp directory is not inside.
- `crates/kerness/tests/common/mod.rs:526` — `config(gameplan, topic, provider)`
  — a `SessionConfig` with `turn_delay: Duration::ZERO`, because the default
  one-second pause between turns is for humans reading a console.
- `crates/kerness/src/testing.rs:16` — `TempDir(pub PathBuf)` — the unit suite's
  copy; `resolved()` canonicalizes for tests that compare a path the access
  policy resolved.
- `bindings/python/tests/conftest.py:9` — `MockProvider`, with
  `PurposeMockProvider`, `SequenceMockProvider` and `CaptureChannel` (`:83`) —
  the Python equivalents, exposed as the `mock_provider` and `capture_channel`
  fixtures.

The eight integration files:

| File | n | What it proves |
| --- | --- | --- |
| `session_run.rs` | 24 | A run end to end: turns, `phase_reached`, `end_reason`, parsed result fields, transcript against channel, per-agent providers, `@MEMORY:` stripped from what is delivered; session defaults filling an agent's unset options, and an agent with its own provider and no model named as an error; a role seating an agent by declaration and never by prose, and a missing role file refused at the `add_agent` that named it |
| `harness_contract.rs` | 16 | Participant bounds collected into one error; `tools:` and `context:` each naming nothing registered, and each narrowing what was; a context source that fails stopping the run before any provider call; `Skill` refused as a tool name; `skills:` unioning; phase rounds clamped; every built-in gameplan declaring `terminate_on` |
| `tools_e2e.rs` | 18 | The tool loop inside a real turn, in all three dialects; unknown tool, schema violation and failing handler each answered as text rather than raised; `MAX_INVALID_CALLS`; `max_tool_iterations`; `tool_results_in_history` both ways; an agent's own `tools` narrowing what it is offered, an empty list leaving it none, a tool it gave up refused at dispatch too, and one the session withheld refused before the run |
| `access_e2e.rs` | 17 | Default-deny; each allow rule; an allowed command still held to the hosts it names; `set_exec` rebuilding the manager; reads outside `allowed_dirs`; `..` denied after resolution; symlink escape; a root confining a read, a write and a command's working directory; an agent root narrowing the session's, and a wider one refused by name |
| `skills_e2e.rs` | 13 | Only name and description reach the prompt; the body arrives for one turn; a repeat load says so; `allowed-tools` narrowing and unioning; `requires-tools` adding back past a gameplan's own list, and refused before the run when nobody registered it |
| `resume.rs` | 12 | A snapshot every turn; a second `run()` continuing; identity mismatch naming the field; bad JSON, wrong version and missing file each handled; captured tool intents requiring reconciliation |
| `compaction_e2e.rs` | 8 | A small ceiling compacting, the anchor turn kept, the count recorded, and history untouched when the summarizer returns nothing |
| `public_api.rs` | 10 | The well-known constants, the shared request defaults, the crate version, the `lib.rs` re-exports, and every built-in asset loading — the Rust half of what the self-check proves for Python; also that a session assembles from the public API alone, that a provider written outside the crate is a `Provider`, and that a reasoning effort round-trips as its name |

## Interactions

- The integration suite compiles against the crate as a dependent does, so it
  transitively covers [session.md](session.md), [run.md](run.md),
  [loop.md](loop.md), [agent-runtime.md](agent-runtime.md),
  [toolkit.md](toolkit.md), [access.md](access.md), [skills.md](skills.md),
  [sessionfile.md](sessionfile.md) and [compaction.md](compaction.md) through
  their public surfaces only.
- `crates/kerness/examples/offline_debate.rs` drives a real `debate` gameplan to
  completion against a scripted provider: no key, no network. CI runs it as a
  smoke test, and it is what a clean clone can run first. `host_control.rs` and
  `resume_approval.rs` are the offline owners of [run.md](run.md)'s host-driven
  and suspended-approval contracts.
- The Python suite and [selfcheck.md](selfcheck.md) cover
  [bindings.md](bindings.md), which nothing on the Rust side can reach.
- `crates/kerness/tests/public_api.rs:40` and
  `bindings/python/tests/test_provider.py` each assert the root's well-known
  constants and the shared request defaults; a constant changed in one language
  fails here before it drifts.
- `release.yml`'s `verify-sdist` job is the only check that the installed
  package, not the working tree, carries every asset and the licence text.

## How to Test

The whole gate, from the repository root:

```sh
cargo fmt --all -- --check                             # pass = exit 0
cargo clippy --workspace --all-targets -- -D warnings  # pass = exit 0
cargo test --workspace -q                              # pass = 407 unit + 118 integration + 1 doctest
cargo test --workspace -q --locked                     # observed: fails on the committed Cargo.lock, see Open Gaps
cargo build -p kerness --examples                      # pass = all 10 compile
cargo run -p kerness --example offline_debate          # pass = completes with no key, exit 0
cargo run -p kerness --example host_control            # pass = validated host result, exit 0
cargo run -p kerness --example resume_approval         # pass = restored approval, each tool once, exit 0
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness   # pass = exit 0
cargo +1.88.0 check --workspace --all-targets --locked # observed: fails on the committed Cargo.lock, see Open Gaps
.venv/bin/python -m pytest bindings/python/tests -q    # pass = 502 passed
.venv/bin/python -m kerness.selfcheck                  # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                   # pass = "All checks passed!"
```

The wheel is built from `bindings/python/`, where `pyproject.toml` lives:

```sh
cd bindings/python && ../../.venv/bin/maturin develop  # pass = installed workspace version
```

Regenerating `Cargo.lock` (any unlocked `cargo` command rewrites the two
workspace entries) makes both `--locked` commands pass on the current tree with
the counts above; the failure is a property of the committed file.

`.github/workflows/ci.yml` runs format, clippy, `cargo test --workspace`
(without `--locked`, `.github/workflows/ci.yml:39`), the example build,
`offline_debate`, rustdoc, the MSRV check with `--locked`
(`.github/workflows/ci.yml:70`), then on Python 3.10 and 3.13 the pytest
suite, self-check and ruff. Rebuild the extension with `maturin develop` before
running the Python suite after a Rust change; `test_packaging.py` catches a
stale one by version.

- Suite-level invariants and their owners: the seam-contention rule at
  `crates/kerness/src/channel.rs:440` and `crates/kerness/src/access.rs:1125`;
  the dependent-compiles rule by every file under `crates/kerness/tests/`; the
  example-rot rule by `bindings/python/tests/test_examples.py:132`; the asset
  pair by `bindings/python/tests/test_packaging.py:42`; the Python module
  inventory by `bindings/python/tests/test_selfcheck.py:18`.
- Gaps: the provider backends are proved only down to the request they build;
  macOS and Windows wheels are built at release time and never tested there.

## Review and Refactor Guide

- Adding a test → the file that already owns the behaviour, chosen by the table
  above; a new integration file is justified only for a new behaviour cluster,
  and it must `mod common;` rather than grow its own doubles.
- Adding a double → `crates/kerness/tests/common/mod.rs` for Rust and
  `bindings/python/tests/conftest.py` for Python. Keep the zero-retry
  construction; a double that retries hides a failure behind a sleep.
- Changing a well-known constant or request default → the crate, the Python
  constructor signature, `crates/kerness/tests/public_api.rs:40` and
  `bindings/python/tests/test_provider.py` in one change.
- Adding a Python module → `_CORE_MODULES` in `bindings/python/kerness/selfcheck.py`,
  or `bindings/python/tests/test_selfcheck.py:18` fails; it must also declare
  `__all__` (`test_packaging.py`).
- Adding a built-in asset → both copies, or
  `bindings/python/tests/test_packaging.py:42` fails; the
  Rust half is `public_api.rs`'s enumeration.
- Adding an example → a Rust example is compiled by CI with no registration; a
  Python example is picked up by `test_examples.py`'s walk, and any gameplan it
  ships must declare `terminate_on`.
- Editing CI → the `msrv` job's toolchain ref must be a released Rust version
  and must match the `rust-version` in `Cargo.toml`; the `--locked` flag there
  is deliberate (the MSRV is a claim about the committed resolution) and must
  stay.
- Forbidden: a test dependency. `tempfile`, `mockall` and the like would break
  the "nothing beyond `Cargo.toml`" posture the two `TempDir` copies exist to
  keep.

Improvement candidates (proposals, not accepted work):

- A Rust-side check that `crates/kerness/assets/` and the package copy are
  byte-identical, so the guard does not depend on the Python surface being
  installed; success check: a `public_api.rs` case that fails when one copy is
  edited.
- Pin the `msrv` job's toolchain to `rust-version` from `Cargo.toml` by reading
  the manifest in the workflow, or add a dependabot `ignore` for that action,
  so a version bump cannot name a toolchain that does not exist; success check:
  the job passes on `main`.

## Open Gaps / Roadmap

- **CI defect:** `.github/workflows/ci.yml:62` pins `dtolnay/rust-toolchain@1.120.0`,
  a Rust version that does not exist, so the "Rust (MSRV 1.88)" job fails at
  `rustup toolchain install` with a 404.
  The intended ref is `1.88.0`, matching `rust-version` in `Cargo.toml`;
  dependabot (`.github/dependabot.yml:11`) will re-bump it unless told to
  ignore that action.
- **Lockfile defect:** the committed `Cargo.lock` records `kerness` and
  `kerness-py` at `0.1.1-dev` while `Cargo.toml` says `0.1.2-dev`, so every
  `--locked` command — the MSRV job's `cargo check --locked` and the gate's
  `cargo test --locked` — fails from a fresh clone until the lock is regenerated
  and committed.
- The integration tests never reach the network, so the four provider backends
  are proved only down to the request they build. Nothing here catches an
  endpoint changing its response shape.
- Examples requiring live provider credentials are compiled (Rust) or parsed
  (Python) without sending requests; external endpoint behaviour stays outside
  the offline suite.
- CI runs on Linux only. The wheels for macOS and Windows are built at release
  time and their tests are not run there, so a platform-specific break arrives
  as a bad wheel rather than a red build.
- Nothing checks that `crates/kerness/assets/` and `bindings/python/kerness/`
  hold the same asset bytes from the Rust side;
  `bindings/python/tests/test_packaging.py:42` is the only guard, and it needs
  the Python surface installed to run.
