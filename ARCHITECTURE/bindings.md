# Bindings

## Goal

The Rust/Python boundary. `kerness._core` is a PyO3 extension module in which
nothing decides anything: every class wraps a type in the `kerness` crate and
every function forwards. What lives here is the translation — JSON values across
the boundary, framework errors as exception instances and back, and Python
callables seen as the traits the framework calls. Serves **M3**.

Above it sits `python/kerness/`, the installed package: one shim per subsystem
that re-exports from `_core`, plus the four modules that are deliberately Python
and the bundled assets.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness-py/src/lib.rs` | the module, `bootstrap`, and the class registry |
| `crates/kerness-py/src/convert.rs` | `serde_json::Value` ↔ Python objects |
| `crates/kerness-py/src/errors.rs` | the exception map, both directions |
| `crates/kerness-py/src/types.rs` | 19 pyclasses: tools, messages, agents, harness specs |
| `crates/kerness-py/src/funcs.rs` | free functions and the framework constants |
| `crates/kerness-py/src/runtime.rs` | `Conversation`, `ToolDispatcher`, `PromptAssembler`, `AgentRunner`, `OrchestratorLoop` |
| `crates/kerness-py/src/{provider,session,access,skill,channel}.rs` | one boundary concern each |
| `python/kerness/__init__.py` | bootstrap and the public surface |
| `python/kerness/<subsystem>.py` | re-export shims, one per crate module |

## Key Types and Entry Points

- `crates/kerness-py/src/lib.rs:34` — `bootstrap(exceptions, dialect, assets_root)` —
  called once from `python/kerness/__init__.py:12`; installs the exception
  classes, the dialect enum, the assets root, the HTTP transport, and the logger.
- `crates/kerness-py/src/lib.rs:48` — `_core(module)` — the `#[pymodule]`; the
  explicit `add_class` list is the extension's whole surface.
- `crates/kerness-py/src/convert.rs:53` — `value_from_py(object)` — Python to
  `Value`; `value_to_py` at `:13` is the inverse. `serde_json` is built with
  `preserve_order`, so a dict's key order survives a round trip.
- `crates/kerness-py/src/errors.rs:146` — `Raise` / `:157` `Catch` — the two
  extension traits that turn `Result<T>` into `PyResult<T>` and back at every
  call site, so no boundary function hand-rolls the mapping.
- `crates/kerness-py/src/types.rs:163` — `PyToolHandler` — a Rust closure seen
  from Python as an ordinary callable, so `spec.handler(...)` works for the
  built-in tools.
- `crates/kerness-py/src/funcs.rs:40` onward — the free functions, and the
  constant block in `register()` that re-exports every framework constant.
- `python/kerness/__init__.py:68` — `__all__` — the public Python surface.

### Three patterns the boundary needs

**Declare the base class in Python, keep the logic in Rust.** `Provider` and
`Channel` are subclassed by user code, so they are Python classes
(`python/kerness/provider.py:64`, `python/kerness/channel.py:17`). Each instance
holds a `_core` handle, and the base-class methods forward `self` back down —
which means a subclass override wins by ordinary Python method resolution, not
by anything the binding does.

**Own the pieces, build the borrowing value transiently.** `PromptAssembler<'a>`
and `AgentRunner<'a>` borrow their inputs, which no `#[pyclass]` can express. The
Python-facing class stores the pieces and constructs the Rust value inside each
call — `crates/kerness-py/src/runtime.rs:210` and `:370`.

**Park the exception.** A framework callback type cannot carry a `PyErr`, so a
raising Python callable's exception is stashed and re-raised at the pyclass
method boundary. See [channel.md](channel.md) for the full account.

## Interactions

- Wraps every module in `crates/kerness/src/`; each subsystem doc names its own
  pyclass.
- Hands the exception hierarchy to [errors.md](errors.md) at bootstrap.
- Installs the patchable HTTP seam described in [provider.md](provider.md).
- Installs the logger described in [channel.md](channel.md).

## How to Test

```sh
cargo clippy --workspace --all-targets -- -D warnings  # pass = exit 0
.venv/bin/maturin develop                              # pass = "Installed kerness-0.1.0"
.venv/bin/python -m pytest tests/ -q                   # pass = 394 passed
```

- The whole `tests/` suite exercises the Python surface only; a gap in the
  binding shows up as a test failure, not as a Rust compile error.
- `tests/test_packaging.py` asserts the version is declared once in
  `pyproject.toml` and once in the package and that the two agree.

## Open Gaps / Roadmap

- No `.pyi` stubs, so editors get no completion or type checking for `_core`.
  The shims re-export names without annotating them.
- `PyToolHandler` exposes a Rust closure as callable but not introspectable;
  `inspect.signature` on it gives the `__call__` signature, not the tool's schema.
- The wheel is built per platform. There is no `sdist`-only fallback for a
  platform without a prebuilt wheel beyond compiling the crate locally.
