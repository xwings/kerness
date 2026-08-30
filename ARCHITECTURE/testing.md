# Testing

## Goal

Three suites, each proving something the other two cannot, and a CI that runs
all of them on every push.

The framework ships as two artifacts, so it can fail in three distinct ways: the
logic can be wrong, the Rust surface can be unusable, or the boundary to Python
can be broken. A suite that catches one of those says nothing about the others.

| Suite | Where | What only it can catch |
| --- | --- | --- |
| Unit | `#[cfg(test)]` inside `crates/kerness/src/` | A function that computes the wrong answer. Reaches internals a caller cannot. |
| Integration | `crates/kerness/tests/` | A session that cannot be *assembled* — a missing re-export, a type a dependent cannot name, a run whose parts are each right and whose whole is not. |
| Python | `bindings/python/tests/` | Anything about the boundary: a value that crosses wrong, a subclass the extension will not accept, an asset the wheel did not install. |

The integration suite is the one that was missing. Before it, every claim about
how a session actually runs — the orchestrator loop, resume, compaction, the
access boundary, the tool dialects — was proved only by driving the framework
through PyO3 from Python, which is a strange way to test a Rust crate and leaves
the pure-Rust caller the crate exists for unrepresented.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/tests/common/mod.rs` | the doubles all eight files share |
| `crates/kerness/tests/*.rs` | one file per behaviour cluster, 101 tests |
| `bindings/python/tests/conftest.py` | the Python suite's equivalent doubles |
| `bindings/python/tests/test_*.py` | 26 modules, 443 tests |
| `crates/kerness/examples/*.rs` | 8 examples, compiled by CI |
| `bindings/python/examples/` | 7 Python examples, walked by `bindings/python/tests/test_examples.py` |
| `.github/workflows/ci.yml` | what runs on push and pull request |
| `.github/workflows/release.yml` | wheels, sdist, and the clean-interpreter check |

No test dependency was added. `common/mod.rs` carries its own temp directory
rather than pulling in `tempfile`, which keeps the "no runtime deps beyond the
Cargo dependencies" posture true of the test build as well.

The unit tests have the same need and cannot see that file — an integration test
is a separate crate, and `common/mod.rs` is compiled into it rather than into
the library. `crates/kerness/src/testing.rs` is the library-side copy, behind
`#[cfg(test)]` so it never reaches the public surface, and the eleven modules
that want a scratch directory share it rather than each writing one. Two copies
is the floor the crate boundary sets; eleven was drift, and it showed as one
module canonicalizing its path and another not.

## Key Types and Entry Points

- `crates/kerness/tests/common/mod.rs:36` — `Call` — one request a double
  received. `system()`, `text()` and `last()` are the three questions tests ask
  of it; `purpose` is how a test tells an orchestrator turn from a participant's.
- `:97` — `ScriptedProvider` — replies written in advance, keyed by purpose
  substring in declaration order, each key owning a sequence with its own cursor
  and a last entry that repeats. Built on `ProviderBase::new(0, 0.0, None)`: zero
  extra attempts, so a scripted reply means exactly one call and a failure
  surfaces instead of being slept over.
- `:271` — `ToolProvider` — emits native tool calls under a chosen
  `ToolDialect`, which is how the OpenAI and Anthropic wire shapes are exercised
  without a network.
- `:365` — `RecordingChannel` — what was delivered, as against what the
  transcript holds. The two differ, and the difference is a tested behaviour.
- `:421` — `TempDir` — `env::temp_dir()/kerness-test-{pid}-{counter}`, removed on
  `Drop`.
- `:499` — `refusal<T>(Result<T>) -> String` — `Session` does not implement
  `Debug`, so `expect_err` is unusable; this is how a test reads a rejection.
- `:510` — `config(gameplan, topic, provider)` — a `SessionConfig` with
  `turn_delay: Duration::ZERO`, because the default one-second pause between
  turns is for humans reading a console.

The eight integration files:

| File | n | What it proves |
| --- | --- | --- |
| `session_run.rs` | 21 | A run end to end: turns, `phase_reached`, `end_reason`, parsed result fields, transcript against channel, per-agent providers, `@MEMORY:` stripped from what is delivered; session defaults filling an agent's unset options, and an agent with its own provider and no model named as an error; a role seating an agent by declaration and never by prose, and a missing role file refused at the `add_agent` that named it |
| `harness_contract.rs` | 13 | Participant bounds collected into one error; `tools:` naming nothing registered; `Skill` refused as a tool name; `skills:` unioning; phase rounds clamped; every built-in gameplan declaring `terminate_on` |
| `tools_e2e.rs` | 13 | The tool loop inside a real turn, in all three dialects; unknown tool, schema violation and failing handler each answered as text rather than raised; `MAX_INVALID_CALLS`; `max_tool_iterations`; `tool_results_in_history` both ways |
| `access_e2e.rs` | 14 | Default-deny; each allow rule; `set_exec` rebuilding the manager; reads outside `allowed_dirs`; `..` denied after resolution; symlink escape; a root confining a read, a write and a command's working directory; an agent root narrowing the session's, and a wider one refused by name |
| `skills_e2e.rs` | 13 | Only name and description reach the prompt; the body arrives for one turn; a repeat load says so; `allowed-tools` narrowing and unioning; `requires-tools` adding back past a gameplan's own list, and refused before the run when nobody registered it |
| `resume.rs` | 9 | A snapshot every turn; a second `run()` continuing; identity mismatch naming the field; bad JSON, wrong version and missing file each handled |
| `compaction_e2e.rs` | 8 | A small ceiling compacting, the anchor turn kept, the count recorded, and history untouched when the summarizer returns nothing |
| `public_api.rs` | 10 | The well-known constants, the shared request defaults, the crate version, the `lib.rs` re-exports, and every built-in asset loading — gameplans, personas, skills, and every role carrying a position and a prompt — the Rust half of what the self-check proves for Python; also that a session assembles from the public API alone, that a provider written outside the crate is a `Provider`, and that a reasoning effort round-trips as its name |

## Interactions

- The integration suite compiles against the crate as a dependent does, so it
  transitively covers [session.md](session.md), [loop.md](loop.md),
  [agent-runtime.md](agent-runtime.md), [toolkit.md](toolkit.md),
  [access.md](access.md), [skills.md](skills.md),
  [sessionfile.md](sessionfile.md) and [compaction.md](compaction.md) through
  their public surfaces only.
- `crates/kerness/examples/offline_debate.rs` drives a real `debate` gameplan to
  completion against a scripted provider: no key, no network. CI runs it as a
  smoke test, and it is what a clean clone can run first.
- The Python suite and [selfcheck.md](selfcheck.md) cover
  [bindings.md](bindings.md), which nothing on the Rust side can reach.
- `release.yml`'s `verify-sdist` job installs the source distribution into a
  clean interpreter *with no checkout beside it*, so an asset the wheel failed to
  include cannot be masked by the working tree. It is also the only check on the
  `LICENSE` and `README.md` symlinks in `bindings/python/`: `license-files` and
  `readme` resolve against that directory, and if they resolve to nothing the
  build still succeeds and simply ships less.

## How to Test

```sh
cargo fmt --all -- --check                            # pass = exit 0
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
cargo test --workspace -q                             # pass = 342 unit + 101 integration, 0 failed
cargo build -p kerness --examples                     # pass = all 8 compile
cargo run -p kerness --example offline_debate         # pass = completes with no key
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 443 passed
```

The wheel is built from `bindings/python/`, where `pyproject.toml` lives:

```sh
cd bindings/python && ../../.venv/bin/maturin develop # pass = "Installed kerness-0.1.0"
```

CI runs those, plus `cargo doc --no-deps -p kerness` under `RUSTDOCFLAGS=-D
warnings`, `cargo check --workspace --all-targets --locked` on the MSRV
toolchain, and `ruff check` over the Python tree.

The MSRV job passes `--locked` deliberately. The MSRV is a claim about the
committed resolution; without the flag a dependency raising its own MSRV fails
the job on a commit that changed nothing here, and the failure names a crate the
project does not own.

## Open Gaps / Roadmap

- The integration tests never reach the network, so the four provider backends
  are proved only down to the request they build. Nothing here catches an
  endpoint changing its response shape.
- Every example except `offline_debate` is compiled but not run, because the
  rest need a key. Compilation catches an API that moved; it does not catch an
  example that runs and does the wrong thing.
- CI runs on Linux only. The wheels for macOS and Windows are built at release
  time and their tests are not run there, so a platform-specific break arrives
  as a bad wheel rather than a red build.
- Nothing checks that `crates/kerness/assets/` and `bindings/python/kerness/` hold the
  same asset bytes from the Rust side; `bindings/python/tests/test_packaging.py:42` is the only
  guard, and it needs the Python surface installed to run.
