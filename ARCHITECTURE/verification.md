---
eatmycode_version: "1.2.0"
---

# Verification and review map detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#verification-and-review-map):
what CI runs, which tests to run first for each kind of change, the review
constraint the owning module enforces, and the coverage gaps.

## Local prerequisites

Use a POSIX environment with a C linker, Rust (declared minimum 1.88), and
Python 3.10+ with virtualenv support. From the repository root, mirror the Python
setup in `.github/workflows/ci.yml:97`:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install 'maturin>=1.7,<2.0' 'ruff>=0.16,<0.17'
(cd bindings/python && ../../.venv/bin/maturin develop --extras dev)
```

The dev extra supplies pytest and Pydantic. Rebuild the extension after Rust
changes. Install Rust 1.88.0 through rustup to run the explicit MSRV command;
Rust formatting and lint checks need rustfmt and Clippy. Dependency installation
needs registry access or cached packages; the test suites and offline examples
need no provider credentials. Run the full checks in the
[root verification map](../ARCHITECTURE.md#verification-and-review-map).

On 2026-09-07, all documented module commands passed: 407 Rust unit tests,
118 integration tests, one doctest and 502 Python tests. A separate source copy
without generated artifacts passed a locked build/test on Rust 1.88.0 and the
Python suite, selfcheck and Ruff after installation into a fresh virtualenv.

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull request:
`rust` (fmt, clippy, test, build examples, `offline_debate`, rustdoc on
stable), `msrv` (`cargo check --workspace --all-targets --locked` on a pinned
toolchain), and `python` (3.10 and 3.13: `maturin develop --extras dev`,
pytest, selfcheck, ruff). The `msrv` job cannot pass while it pins
`dtolnay/rust-toolchain@1.120.0`; see [Roadmap](../ARCHITECTURE.md#roadmap).
`release.yml`'s `verify-sdist` job installs the source distribution into a
clean interpreter with no checkout and runs the self-check; it is the only
check on the `bindings/python/{LICENSE,README.md}` symlinks and on asset
packaging ([testing.md](testing.md)).

## Change to test map

`pytest` paths are relative to `bindings/python/`; run them as
`.venv/bin/python -m pytest bindings/python/<path>` from the repository root.

| If you change | Run first | Then | Review constraints |
| --- | --- | --- | --- |
| Frontmatter keys or validation | `cargo test -p kerness -- harness yaml`; `--test harness_contract` | `pytest tests/test_harness.py` | every accepted key must be validated, rendered or enforced ([harness.md](harness.md)) |
| Access rules, paths, commands | `cargo test -p kerness -- access exec::`; `--test access_e2e` | `pytest tests/test_access.py` | direct tests at the boundary, default deny, narrowing only ([access.md](access.md)) |
| Provider payloads, retries, usage | `cargo test -p kerness --lib -- provider usage`; `--test tools_e2e` | `pytest tests/test_provider.py` | constants asserted on both sides; `chat` is one request ([provider.md](provider.md)) |
| Turn stepping, tool loop | `cargo test -p kerness --lib agent_runtime`; `--test tools_e2e` | `pytest tests/test_agent_runtime.py` | continuation is owned data; counters saturate ([agent-runtime.md](agent-runtime.md)) |
| Scheduling, phases, end reasons | `cargo test -p kerness --lib orchestrator` | `pytest tests/test_loop.py` | forward-only phases; no IO in the loop ([loop.md](loop.md)) |
| Run engine, approvals, budgets, outcomes | `cargo test -p kerness --test session_run --test resume --test compaction_e2e --test public_api`; the two control examples | `pytest tests/test_session.py`; `examples/host_control.py` | one operation per step; intent before effect ([run.md](run.md)) |
| Session file schema | `cargo test -p kerness --lib sessionfile`; `--test resume` | `pytest tests/test_sessionfile.py` | schema 2 written, valid v1 read; unknown fields rejected ([sessionfile.md](sessionfile.md)) |
| Memory stores or filter | `cargo test -p kerness --lib -- memory usage`; `--test session_run memory_maintenance` | `pytest tests/test_memory.py tests/test_session.py` | filter before store; scopes close once ([memory.md](memory.md)) |
| Prompt assembly, context, skills index | `cargo test -p kerness -- prompting context skill` | `pytest tests/test_prompting.py tests/test_skill_*.py` | fixed part order; caveat on memory only ([prompting.md](prompting.md)) |
| Tool specs, dialects, schemas | `cargo test -p kerness -- tooling toolkit toolschema jsonschema` | `pytest tests/test_tool*.py tests/test_jsonschema.py` | `Skill` reserved; wire shapes exact ([toolschema.md](toolschema.md)) |
| Channels, logging seams | `cargo test -p kerness channel` | `pytest tests/test_channel.py` | exact-type shortcut; parked exceptions ([channel.md](channel.md)) |
| Any `bindings/python/src/*.rs` | `cargo clippy --workspace --all-targets -- -D warnings`; `maturin develop` | full pytest, selfcheck, ruff | forwarding only; no policy in Python ([bindings.md](bindings.md)) |
| Any bundled asset | `cargo test -p kerness --test public_api` | `pytest tests/test_packaging.py tests/test_selfcheck.py` | edit both copies; stay framework-generic |
| A public constant or default | `cargo test -p kerness --test public_api` | `pytest tests/test_provider.py tests/test_packaging.py` | update the [constant table](runtime.md#well-known-constants) in the same change |
| Any public Rust API | `cargo build -p kerness --examples`; rustdoc | `pytest tests/test_examples.py` | re-exports in `lib.rs`; examples must still compile |

## Coverage gaps

- No test reaches the network; the four backends are proved down to the request
  they build ([provider.md](provider.md)).
- CI runs on Linux only; macOS and Windows wheels are built at release time
  without running the suites there, and Windows is not a declared platform.
- `PyStore::revise` is the one binding crossing no test drives
  ([memory.md](memory.md)).
- Nothing checks the assets pair from the Rust side; the guard needs the Python
  surface installed.
- Relative line references in these docs are not machine-checked.
