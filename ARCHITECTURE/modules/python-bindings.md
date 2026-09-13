---
eatmycode_version: "2.0.0"
---

# Python bindings

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing any PyO3 wrapper, Python API/shim/callback, installed-package
diagnostic, Python test/example, wheel metadata or asset packaging mechanics.

## Responsibility and Status

Implemented: `kerness._core` exposes the Rust kernel through Python classes,
callables and values. The package has no independent session engine. Owns all
binding source/tests/examples/configuration; asset content semantics remain with
harness/assets. Verification checks the installed extension and exports. Declared
CPython >=3.10 support exceeds the interpreter exercised locally.

## Code Map

Paths are under `bindings/python/`.

| Path / symbol | Role |
| --- | --- |
| [src/lib.rs](../../bindings/python/src/lib.rs), `bootstrap`; [kerness/__init__.py](../../bindings/python/kerness/__init__.py) | Register exceptions/dialect/assets and install transport/output/prompt seams before re-exports. |
| [src/session.rs](../../bindings/python/src/session.rs), `PySession`; [src/run.rs](../../bindings/python/src/run.rs), `PySessionRun` | Construction, owned inputs/outcomes/events, cancellation and contextual callbacks. |
| [src/provider.rs](../../bindings/python/src/provider.rs); [kerness/provider.py](../../bindings/python/kerness/provider.py), `Provider` | Trait adapter, Python override/signature inspection and optional Pydantic validation. |
| [src/convert.rs](../../bindings/python/src/convert.rs), [src/errors.rs](../../bindings/python/src/errors.rs); remaining `src/` adapters and `kerness/*.py` | Value/error translation, PyO3 types, ABCs/dataclass/enum and re-export shims. |
| [kerness/selfcheck.py](../../bindings/python/kerness/selfcheck.py), `main`; [pyproject.toml](../../bindings/python/pyproject.toml), [Cargo.toml](../../bindings/python/Cargo.toml); `tests/`, `examples/` | Installed imports/assets diagnostics, build metadata and boundary consumers. |

## Local Conventions

Required project boundary ([README](../../README.md#two-artifacts-one-kernel),
`src/lib.rs`): runtime behavior is implemented in Rust. Python runtime glue is
limited to subclassable classes, declarations the extension cannot provide
(exceptions/enum/dataclass), signature inspection, Pydantic validation and
re-exports. `selfcheck.py` separately owns package diagnostics. New glue must
name its interpreter requirement.

Observed wrappers use `Py*` Rust names and public `#[pyclass(name=..., module=...)]`
names. ABCs register bundled crate implementations; `ToolDialect` is a Python
enum with stable identity. Public modules declare resolvable `__all__`, enforced
by `tests/test_packaging.py`. Ruff configuration in `pyproject.toml` is required;
bootstrap-dependent imports intentionally use E402 exemption. Partial type hints
are observed; no Python type checker is configured. Match existing pytest
fixtures in `tests/conftest.py` and patched `kerness.provider.http_post_json`.

## Contracts and Invariants

- Import bootstraps exceptions, enum identity and installed assets before shims
  import extension names. Interpreter-backed stdout/logger/prompt/HTTP replace
  delivery seams, preserving Rust decisions (`src/lib.rs`, `__init__.py`).
- `convert.rs`/`errors.rs` translate values and recognized exception fields in
  both directions. Preserve ordered JSON maps and the errors represented by each
  callback contract; unsupported exception classes can lose their original type
  (see Known Gaps). A Rust error must not become silent Python success.
- `session.start(mode="host_driven")` returns an owned run; `step` accepts tagged
  dictionaries and returns progress/waiting/finished dictionaries matching Rust
  serialization. An event sink receives one event dictionary (`src/run.rs`).
- Contextual handlers receive `(arguments, context)`; optional preflight receives
  `(arguments, identity)` and returns `None` or a declared action dictionary.
  The detached context handle retains engine identity but expires after the
  invocation. Preflight cannot execute the action. Blocking kernel work releases
  the GIL where wrappers declare it; callbacks reacquire it (`src/run.rs`).
- Provider subclasses can override supplied behavior. Signature inspection only
  decides which Python keywords can be forwarded; retry/fallback policies stay
  in Rust. Pydantic is optional via `structured`; schema/response validation must
  not alter plain-text or tool-only behavior (`kerness/provider.py`).
- Workspace version is the artifact version source; Python exports the compiled
  extension's version. PyO3 uses `abi3-py310`. Package README/LICENSE symlinks
  supply metadata. Bundled asset copies must agree with Rust; build rules exclude
  stale bytecode (`pyproject.toml`, `tests/test_packaging.py`).

## Dependencies and Boundaries

Bindings depend on the kernel; the kernel never imports Python. When changing an
exposed behavior, first read the relevant [runtime](runtime.md),
[provider](providers.md), [tool/access](tools-access.md),
[memory](memory-persistence.md) or [harness](harness-assets.md) owner. They own
behavior; this module owns translation. Read [build checks](../topics/build-checks.md)
before manifest/CI changes or a full rebuild. For bundled asset content read
harness/assets; for inclusion paths and parity-test mechanics stay here.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| Public class/function/value/error | Adapter, shim, root exports and matching test | Read Rust behavior owner; update this owner and run installed-package tests. |
| Owned inputs/events/context callback | `src/run.rs`, `src/session.rs`, `tests/test_session.py` | Read runtime/tools-access; assert tags, error fields, cancellation and expired handles. |
| Provider override/structured output | `src/provider.rs`, `kerness/provider.py`, `tests/test_provider.py` | Read providers; patched transport, subclass and Pydantic cases. |
| Packaging/module/asset family/example | Manifests, `selfcheck.py`, discovery/parity tests, example | Read build checks and harness/assets for content; rebuild before testing exports. |

## Verification

Follow [build/install prerequisites](../topics/build-checks.md#contract), then
run the [root Python commands](../../ARCHITECTURE.md#verification). Pytest checks
FFI behavior; `test_packaging.py` verifies version, asset equality and every public
export; `test_selfcheck.py` verifies diagnostic coverage. Selfcheck success alone
does not prove session behavior. From root also run:

`.venv/bin/python bindings/python/examples/host_control.py`

It must drive an offline host-controlled run through the installed engine.
Rust checks remain required for adapter edits. See
[baseline evidence](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

`src/errors.rs::to_py` maps Rust `Error::Io` to Python `OSError`, but `from_py`
has no corresponding `OSError` branch and falls back to `Error::Session`.
Arbitrary Python exception classes also fall through that path; callbacks do not
promise exact exception-type round trips. This is an existing conversion gap,
not a new exception-preservation guarantee.

No independent Python type-checking gate or full interpreter/platform matrix is
available locally. A stale `_core` may make source edits invisible; rebuild before
diagnosing such failures. Most endpoint tests mock HTTP and do not prove live
provider behavior. Asset synchronization is manual, guarded by parity tests.
