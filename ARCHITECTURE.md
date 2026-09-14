---
eatmycode_version: "2.0.0"
---

# Kerness Architecture

## Read First

Before planning code changes or reviewing code, read
[Agent Rules](ARCHITECTURE/AGENT_RULES.md). Follow the Task Index to the
owning module and read pages whose **Read when** trigger matches the task.
Load partner modules only for affected boundaries; never load the entire
ARCHITECTURE directory. Reuse unchanged pages already read in this session.
Check claims against source, configuration, and tests; they remain
authoritative. If a route or fact is missing or stale, inspect source and
repair the affected docs. For broad changes, work through owners in batches
and retain cross-owner constraints and verification evidence.

## Project Snapshot

| Fact | Value and evidence |
| --- | --- |
| Purpose | Markdown-driven multi-agent sessions; Rust and Python share one kernel ([entry](crates/kerness/src/lib.rs)). |
| Toolchain | Rust 2021/MSRV 1.88, workspace version 0.1.2-dev ([Cargo](Cargo.toml)); Python >=3.10, PyO3 0.23/abi3-py310, maturin >=1.7,<2 ([Python](bindings/python/pyproject.toml), [binding](bindings/python/Cargo.toml)). |
| Platforms | Linux/macOS declared; POSIX access/process assumptions. CI targets Ubuntu/Python 3.10 and 3.13; MSRV/platform gaps are in [build checks](ARCHITECTURE/topics/build-checks.md). |
| Non-goals | No daemon, async scheduler, streaming, exact tokenizer or model-price registry. Hosts supply windows/prices; token/cost limits are measured thresholds (provider/memory owners below). |

## System Design

`Session` validates the gameplan/defaults; `SessionRun` owns scheduling, turns,
approvals and outcomes, including optional concurrent batches. Tool effects run
serially in private turns before conversation updates. Snapshots persist state; hosts
rebind callbacks on resume ([runtime](ARCHITECTURE/modules/runtime.md)).

The kernel has no Python dependency; bindings translate values and install
interpreter delivery seams ([bootstrap](bindings/python/src/lib.rs)). Model
commands/files cross access policy; host callbacks/provider HTTP are outside it.
Memory is quoted data. Agent workspaces only narrow session grants. Contract
keys must be consumed by validation, rendering or execution
([contract tests](crates/kerness/tests/harness_contract.rs)).

## Code Conventions

| Standing | Convention and evidence |
| --- | --- |
| Required | Runtime features live in Rust; Python supplies interpreter glue/re-exports ([README](README.md#two-artifacts-one-kernel), [binding rules](ARCHITECTURE/modules/python-bindings.md#local-conventions)). |
| Configured | Default rustfmt, Clippy/rustdoc warnings fail; Ruff E4/E7/E9/F, py310, bootstrap E402 exemption. No Python formatter/type checker ([CI](.github/workflows/ci.yml), [pyproject](bindings/python/pyproject.toml)). |
| Observed | Rust snake_case functions/files, CamelCase types, uppercase constants; `//!`/`///` docs, logging seams, trait/closure composition ([session](crates/kerness/src/session.rs)). Python `Py*` wrappers, ABCs, explicit `__all__`; pytest behavior names. |
| Established project additions | IO uses crate `Error`/`Result`; docs describe current contracts; accepted harness keys need consumers; bundled assets stay generic/equal; tests discover assets/modules and directly exercise security boundaries ([harness](ARCHITECTURE/modules/harness-assets.md), [access tests](crates/kerness/tests/access_e2e.rs), [packaging tests](bindings/python/tests/test_packaging.py)). |
| Artifacts | Maintain both asset copies. Cargo manages `Cargo.lock`; maturin rebuilds `_core`; never edit generated binaries/caches ([.gitignore](.gitignore)). |

## Verification

Commands run at root. Read [build checks](ARCHITECTURE/topics/build-checks.md)
for setup, manifests/CI, fixtures, doc routing or both-language validation.

| Change/check | Command and working directory | Prerequisites / pass evidence |
| --- | --- | --- |
| Rust suite | `cargo test --workspace` | Rust >=1.88; tests pass. |
| Rust style/build | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings` | rustfmt/Clippy; exit 0. |
| Local run | `cargo run -p kerness --example offline_debate` | No credentials; completed session. |
| Python | `.venv/bin/python -m pytest bindings/python/tests -q`; `.venv/bin/python -m kerness.selfcheck`; `.venv/bin/ruff check bindings/python` | Linked setup first; tests, OK selfcheck, lint pass. |

## Task Index

Paths below `src/` mean `crates/kerness/src/`; module maps assign shared tests.

| Source paths / task trigger | Responsibility | Read next |
| --- | --- | --- |
| `src/session.rs`, `src/session/`, `agent*`, `orchestrator`, `prompting`, `context`, `channel`; core utilities/public API, Rust examples | Session execution and core composition | [Runtime](ARCHITECTURE/modules/runtime.md) |
| `src/{harness,gameplan,role,persona,assets,yaml}.rs`, `skill/{mod,loader}.rs`; both bundled asset trees | Declarative contracts and asset loading | [Harness/assets](ARCHITECTURE/modules/harness-assets.md) |
| `src/{access,exec,tooling,toolkit,jsonschema}.rs`, `skill/runtime.rs`; access/tool/skill integration tests | Tool execution and permissions | [Tools/access](ARCHITECTURE/modules/tools-access.md) |
| `src/provider/`, `http.rs`, `toolschema.rs`, `usage.rs`; transport/dialects/accounting | Model I/O | [Providers](ARCHITECTURE/modules/providers.md) |
| `src/{memory,compaction,conversation,sessionfile}.rs`; compaction/resume tests | State and persistence | [Memory/persistence](ARCHITECTURE/modules/memory-persistence.md) |
| `bindings/python/` except asset content; Python APIs/tests/examples/package config | Interpreter boundary | [Python bindings](ARCHITECTURE/modules/python-bindings.md) |
| Workspace/core manifests/lockfile, `.github/`, shared test helpers, architecture/agent aliases, README, root `assets/`/LICENSE | Build, verification and project docs | [Build checks](ARCHITECTURE/topics/build-checks.md) |
