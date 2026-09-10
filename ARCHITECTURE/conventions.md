---
eatmycode_version: "1.2.0"
---

# Coding conventions detail

Supporting page for [ARCHITECTURE.md](../ARCHITECTURE.md#coding-style-and-code-design):
the enforced rules with their configuration and command, the observed Rust and
Python conventions with the sites to copy, and what nothing enforces.

## Enforced

| Rule | Tool and configuration | Command |
| --- | --- | --- |
| Rust formatting | `rustfmt` defaults; no `rustfmt.toml` | `cargo fmt --all -- --check` |
| Rust lint | `clippy` on every target, warnings are errors | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust docs | `rustdoc` warnings are errors | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p kerness` |
| MSRV | `rust-version = "1.88"` in `Cargo.toml` | `cargo +1.88.0 check --workspace --all-targets --locked` |
| Python lint | `ruff` with `E4, E7, E9, F`, `py310`; `E402` ignored in `kerness/__init__.py` because `bootstrap` must run first (`pyproject.toml`) | `.venv/bin/ruff check bindings/python` |
| Shim shape | every public module declares `__all__` and every name resolves (`bindings/python/tests/test_packaging.py:74`, `:83`); every module is in the self-check list (`bindings/python/tests/test_selfcheck.py:18`) | pytest |
| Constants | the well-known constants and request defaults agree across the boundary (`crates/kerness/tests/public_api.rs:43`, `:70`; `bindings/python/tests/test_provider.py`) | test suites |

## Observed conventions

**Rust.**

- Every `.rs` file opens with a `//!` module doc stating what the module owns
  and why it is shaped that way; public items carry `///` docs. Rationale lives
  in those comments, not in a changelog.
- Naming is `snake_case` functions, `CamelCase` types, `SCREAMING_SNAKE` constants;
  the binding's pyclasses are `Py<Name>` with `#[pyclass(name = "Name", module = "kerness._core")]`
  plus `frozen`, `get_all`, `set_all` or `subclass` as needed (`bindings/python/src/types.rs`).
- Errors: `crate::error::{Error, Result}` everywhere IO or a provider is
  touched. Process-wide lock slots use `expect("... lock poisoned")`, while
  session, memory and usage mutexes recover poisoned guards
  (`crates/kerness/src/session.rs:373`, `crates/kerness/src/memory.rs:576`,
  `crates/kerness/src/usage.rs:365`). The only bare
  `.unwrap()` calls in non-test code are 13 state-machine invariants in
  `crates/kerness/src/session/run.rs`. `unreachable!` carries a sentence saying
  why.
- `#[allow]` is rare and local: `clippy::too_many_arguments` on PyO3
  constructors (`bindings/python/src/types.rs:685`, `runtime.rs:417`,
  `session.rs:60`, `provider.rs:174` and their siblings) and a commented
  `clippy::large_enum_variant` at `crates/kerness/src/session/run.rs:164`.
- `unsafe` appears only at `crates/kerness/src/exec.rs:159`, `:163`, `:200`
  and `:370`, for `libc::fcntl` and `libc::kill`, under `#[cfg(unix)]`.
- No `println!`/`eprintln!` outside the default sink in
  `crates/kerness/src/logging.rs`; diagnostics go through `logging::{debug,warning,error}`
  and console output through `ConsoleWriter`.
- Serde: checkpoint and continuation types carry
  `#[serde(deny_unknown_fields)]` (19 sites across `agent_runtime`,
  `orchestrator`, `session/*`, `usage`); tagged enums use
  `#[serde(tag = "...", rename_all = "snake_case")]`; `serde_json` is built with
  `preserve_order` so dict order survives the boundary.
- Process-wide seams follow one shape: `fn slot() -> &'static RwLock<...>` over
  a `OnceLock`, a `set_*` installer, and a default that works from Rust alone
  (`http.rs`, `logging.rs`, `channel.rs`, `access.rs`, `assets.rs`).
- Free functions generic over `P: Provider + ?Sized` carry supplied trait
  behaviour so a Python override wins ([provider.md](provider.md)).
- Tests are named as sentences in `snake_case`
  (`the_documented_constants_hold_their_documented_values`). Unit tests sit in
  `#[cfg(test)] mod tests` in 30 of the 42 files under `crates/kerness/src/`
  and share `crates/kerness/src/testing.rs` for scratch directories; the four
  backend files are tested from `provider/mod.rs`, and `session/run.rs`,
  `session/capabilities.rs`, `session/outcome.rs`, `skill/mod.rs`, `http.rs`,
  `logging.rs`, `lib.rs` and `testing.rs` carry none and are proved from
  `crates/kerness/tests/` and the Python suite. Integration tests share the
  doubles in `crates/kerness/tests/common/mod.rs` and add no test dependency.

**Python.**

- A shim is a docstring, `from kerness._core import ...`, and `__all__`
  (`bindings/python/kerness/toolkit.py`). Anything more is a finding unless it
  is one of the five permitted runtime-glue kinds. The Python
  [self-check](selfcheck.md) separately owns installation diagnostics.
- Subclassable bases are `abc.ABC` with `@abstractmethod`; bundled crate types
  are registered against them with `ABC.register`
  (`bindings/python/kerness/channel.py:49`, `memory.py:137`).
- `AccessPolicy` is a `@dataclass` with list defaults
  (`bindings/python/kerness/access.py:21`); `ToolDialect` is an `enum.Enum`
  compared with `is`.
- Tests: pytest, `class Test<Behaviour>` grouping with `test_<sentence>`
  functions, shared `MockProvider` and `PurposeMockProvider` in
  `bindings/python/tests/conftest.py`, transport patched at
  `kerness.provider.http_post_json`.

## Unenforced or inconsistent

- No Python type checker runs; annotations are partial and untested.
- `auto_approve_prefixes` has no doc comment on either surface
  (`crates/kerness/src/access.rs:188`, `bindings/python/kerness/access.py:36`)
  while every sibling field does.
- Relative `:NN` references in module docs are a documentation convention only;
  nothing in CI checks them.
