# Bindings

## Goal

The Rust/Python boundary. `kerness._core` is a PyO3 extension module in which
nothing decides anything: every class wraps a type in the `kerness` crate and
every function forwards. What lives here is the translation — JSON values across
the boundary, framework errors as exception instances and back, and Python
callables seen as the traits the framework calls.

Above it sits `bindings/python/kerness/`, the installed package: one shim per
subsystem that re-exports from `_core`, the handful of declarations that cannot
be made from Rust — the three subclassable base classes, the exception
hierarchy, `ToolDialect`, `AccessPolicy` — and the bundled assets. No feature is
implemented here; `ARCHITECTURE.md` states the rule and lists the four seams
that keep it true when a feature needs the interpreter.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `bindings/python/src/lib.rs` | the module, `bootstrap`, and the class registry |
| `bindings/python/src/convert.rs` | `serde_json::Value` ↔ Python objects |
| `bindings/python/src/errors.rs` | the exception map, both directions |
| `bindings/python/src/types.rs` | 21 pyclasses: tools, messages, agents, roles, harness specs |
| `bindings/python/src/funcs.rs` | free functions and the framework constants |
| `bindings/python/src/runtime.rs` | `Conversation`, `ToolDispatcher`, `PromptAssembler`, `AgentRunner`, `OrchestratorLoop` |
| `bindings/python/src/{provider,session,access,skill,channel,memory}.rs` | one boundary concern each |
| `bindings/python/kerness/__init__.py` | bootstrap and the public surface |
| `bindings/python/kerness/<subsystem>.py` | re-export shims, one per crate module |
| `bindings/python/pyproject.toml` | the wheel's manifest; a Python build starts here |

## Key Types and Entry Points

- `bindings/python/src/lib.rs:34` — `bootstrap(exceptions, dialect, assets_root)` —
  called once from `bindings/python/kerness/__init__.py:12`; installs the exception
  classes, the dialect enum, the assets root, and then all four seams — the HTTP
  transport, the console writer, the logger, and the console prompt.
- `bindings/python/src/lib.rs:50` — `_core(module)` — the `#[pymodule]`; the
  explicit `add_class` list is the extension's whole surface.
- `bindings/python/src/convert.rs:59` — `value_from_py(object)` — Python to
  `Value`; `value_to_py` at `:15` is the inverse. `serde_json` is built with
  `preserve_order`, so a dict's key order survives a round trip.
- `bindings/python/src/errors.rs:146` — `Raise` / `:157` `Catch` — the two
  extension traits that turn `Result<T>` into `PyResult<T>` and back at every
  call site, so no boundary function hand-rolls the mapping.
- `bindings/python/src/types.rs:165` — `PyToolHandler` — a Rust closure seen
  from Python as an ordinary callable, so `spec.handler(...)` works for the
  built-in tools.
- `bindings/python/src/funcs.rs:46` onward — the free functions, and the
  constant block in `register()` at `:674` that re-exports every framework
  constant.
- `bindings/python/src/funcs.rs:679` — `__version__` — `env!("CARGO_PKG_VERSION")`
  at the top of that block. `kerness/__init__.py:14` re-exports it, so the
  number a caller reads is the one the binary was compiled at rather than a
  literal that can drift from it.
- `bindings/python/kerness/__init__.py:73` — `__all__` — the public Python surface.

### Three patterns the boundary needs

**Declare the base class in Python, keep the logic in Rust.** `Provider`,
`Channel`, and `MemoryStore` are subclassed by user code, so they are Python
classes (`bindings/python/kerness/provider.py:99`,
`bindings/python/kerness/channel.py:21`,
`bindings/python/kerness/memory.py:27`). `Provider` holds a `_core` handle and
its methods forward `self` back down, so a subclass override wins by ordinary
Python method resolution rather than by anything the binding does. The other two
have no logic to forward — the four bundled channels and the two bundled stores
are crate types registered against their ABC at `channel.py:50` and
`memory.py:86`, so `isinstance` holds without inheritance.

**Own the pieces, build the borrowing value transiently.** `PromptAssembler<'a>`
and `AgentRunner<'a>` borrow their inputs, which no `#[pyclass]` can express.
The Python-facing class stores the pieces and constructs the Rust value inside
each call — `bindings/python/src/runtime.rs:266` and `:474`.

**Park the exception.** A framework callback type cannot carry a `PyErr`, so a
raising Python callable's exception is stashed and re-raised at the pyclass
method boundary. See [channel.md](channel.md) for the full account.

### Five ways in, and one that fails closed

Python code reaches the framework as five traits, and each is bound at a
different moment. `Provider`, `Channel`, and `MemoryStore` are subclassed, so
the binding takes the instance — a store through `bind_memory_store`
(`bindings/python/src/memory.rs:136`), which passes a bundled store straight
through rather than round-tripping every call through the interpreter.
A tool handler is any callable and is bound by `Session.add_tool`. A context
source is a callable too, bound by `add_context`
(`bindings/python/src/session.rs:524`), called once per agent at the top of
`run()` rather than per turn. And a memory filter is bound by
`bind_memory_filter` (`:206`), which refuses a non-callable at construction
rather than at the first note an agent writes.

`PyFilter` (`:176`) is the one that fails closed rather than parking. A filter
that raises drops the note and logs a warning: the filter is a trust boundary
([memory.md](memory.md)), and a boundary that lets a note through because the
check crashed is not one. The parked-exception pattern would surface the error
to the caller, which is the right answer for a channel and the wrong one here —
by the time `run()` returned, the note would already be in the file.

An agent's `tools` (`bindings/python/src/types.rs:851`) and the access policy's
`allowed_hosts` (`bindings/python/src/access.rs:203`) are the mirror direction:
plain data, extracted at the boundary, validated by the crate.

## Interactions

- Wraps every module in `crates/kerness/src/`; each subsystem doc names its own
  pyclass.
- Hands the exception hierarchy to [errors.md](errors.md) at bootstrap.
- Installs the patchable HTTP seam described in [provider.md](provider.md).
- Installs the console writer and the logger described in
  [channel.md](channel.md), and the console prompt described in
  [access.md](access.md).

## How to Test

```sh
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 487 passed
cd bindings/python && ../../.venv/bin/maturin develop # pass = "Installed kerness-0.1.0"
```

- The whole `bindings/python/tests/` suite exercises the Python surface only; a
  gap in the binding shows up as a test failure, not as a Rust compile error.
- `bindings/python/tests/test_packaging.py` asserts the installed package
  reports the root `Cargo.toml`'s `[workspace.package] version`, which is what
  catches a stale extension left in `site-packages` after a bump.

## Open Gaps / Roadmap

- No `.pyi` stubs, so editors get no completion or type checking for `_core`.
  The shims re-export names without annotating them.
- `PyToolHandler` exposes a Rust closure as callable but not introspectable;
  `inspect.signature` on it gives the `__call__` signature, not the tool's schema.
- The wheel is built per platform. There is no `sdist`-only fallback for a
  platform without a prebuilt wheel beyond compiling the crate locally.
