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

`done` — the legacy and M1–M3 owned-run/contextual-tool APIs forward to the Rust
engine. The rebuilt extension passes all 502 Python tests, selfcheck, Ruff, and
the offline host-control example.

## Code Structure

| File | Role |
| ---- | ---- |
| `bindings/python/src/lib.rs` | the module, `bootstrap`, and the class registry |
| `bindings/python/src/convert.rs` | JSON values and rendered chat messages ↔ Python objects |
| `bindings/python/src/errors.rs` | the exception map, both directions |
| `bindings/python/src/types.rs` | 21 pyclasses: tools, messages, agents, roles, harness specs |
| `bindings/python/src/funcs.rs` | free functions and the framework constants |
| `bindings/python/src/runtime.rs` | `Conversation`, `ToolDispatcher`, `PromptAssembler`, `AgentRunner`, `OrchestratorLoop` |
| `bindings/python/src/run.rs` | `SessionRun`, `RunControl`, `ToolContext`, and contextual/preflight/event callback translation |
| `bindings/python/src/{provider,session,access,skill,channel,memory}.rs` | one boundary concern each |
| `bindings/python/kerness/__init__.py` | bootstrap and the public surface |
| `bindings/python/kerness/<subsystem>.py` | re-export shims, one per crate module |
| `bindings/python/pyproject.toml` | the wheel's manifest; a Python build starts here |

## Key Types and Entry Points

- `bindings/python/src/lib.rs:36` — `bootstrap(exceptions, dialect, assets_root)` —
  called once from `bindings/python/kerness/__init__.py:12`; installs the exception
  classes, the dialect enum, the assets root, and then all four seams — the HTTP
  transport, the console writer, the logger, and the console prompt.
- `bindings/python/src/lib.rs:52` — `_core(module)` — the `#[pymodule]`; the
  explicit `add_class` list is the extension's whole surface.
- `bindings/python/src/convert.rs:59` — `value_from_py(object)` — Python to
  `Value`; `value_to_py` at `:15` is the inverse. `serde_json` is built with
  `preserve_order`, so a dict's key order survives a round trip.
  `chat_message_to_py` at `:113` owns the shared `role`/`content` dictionary
  conversion for single turns, conversations, and summary requests.
- `bindings/python/src/errors.rs:146` — `Raise` / `:157` `Catch` — the two
  extension traits that turn `Result<T>` into `PyResult<T>` and back at every
  call site, so no boundary function hand-rolls the mapping.
- `bindings/python/src/types.rs:167` — `PyToolHandler` — a Rust closure seen
  from Python as an ordinary callable, so `spec.handler(...)` works for the
  built-in tools.
- `bindings/python/src/funcs.rs:47` onward — the free functions, and the
  constant block in `register()` at `:672` that re-exports every framework
  constant.
- `bindings/python/src/funcs.rs:677` — `__version__` — `env!("CARGO_PKG_VERSION")`
  at the top of that block. `kerness/__init__.py:14` re-exports it, so the
  number a caller reads is the one the binary was compiled at rather than a
  literal that can drift from it.
- `bindings/python/kerness/__init__.py:80` — `__all__` — the public Python surface.
- `bindings/python/src/session.rs:633` — `PySession::start` — consumes the
  prepared configuration and returns an owned run; contextual registration is
  `add_contextual_tool` at `:556`.
- `bindings/python/src/run.rs:190` — `PySessionRun` — step/control/checkpoint
  forwarding; `PyToolContext` at `:106` carries invocation capabilities.

### Owned execution and contextual tools

`Session.start(*, mode="automatic", approvals="external", budget=None,
pricing=None, event_sink=None, result_validation="strict", binding_version="")`
transfers the Rust session into a `SessionRun`. The Python session retains an
empty slot afterward; further configuration or execution through that object
raises `SessionError` naming the existing run handle. Invalid Python argument
conversion is rejected before transfer; a Rust preparation failure consumes the
configuration, matching the Rust API. Existing `Session.run()` keeps its
blocking interface and leaves the session available afterward.

`SessionRun.step(input=None)` deserializes a JSON-compatible dictionary into
Rust `RunInput`; `None` is `continue`. Its returned dictionary is the serialized
Rust `StepOutcome`. Inputs use a `kind` field, such as `select_agent`,
`user_message`, `approve`, `reconcile`, and `finish`; outcomes use `status`:
`progress`, `waiting`, or `finished`. The engine validates and executes every
transition. `outcome()`, `usage()`, and `drain_events()` use the same JSON value
conversion; `checkpoint()` forwards to the engine. A `control()` handle can
request cooperative cancellation without borrowing the running object. `step`
and contextual command execution release the GIL while Rust is running;
Python callbacks acquire it when invoked. Cancellation therefore remains
available to another Python thread without introducing a framework thread.

An optional `event_sink(event)` receives each Rust event as a dictionary.
Events observe the run; decisions enter through `step` or the control handle.
The Rust runtime stops on a sink error and does not replay a completed tool to
redeliver an event. Python's mutable borrow rejects reentrant `step()` calls.
`budget`, `pricing`, and `binding_version` are forwarded to Rust; no scheduling,
pricing, access, recovery, or budget policy lives in Python.

`Session.add_tool_spec(spec)` preserves `ToolSpec.takes_actor`.
`add_contextual_tool(name, description, parameters, handler, *, preflight=None)`
is additive: legacy `handler(arguments)` remains unchanged, while a contextual
handler receives `(arguments, context)`. An optional preflight receives
`(arguments, identity_dict)` and returns `None` or the Rust action shape:
`{"kind": "confirm", "description": "..."}` or
`{"kind": "command", "command": "...", "cwd": null}`. It must be free of
side effects, and an arbitrary synchronous callback cannot suspend mid-stack.

`ToolContext` owns a clone of the Rust capability handle, with read-only actor,
run ID, numeric turn ID, and call ID properties. The call ID is the engine's
correlation ID, not the provider's native tool-call ID. Its file, directory, command,
and memory methods use that actor's policy without borrowing `PySession`.
Capabilities expire when the invocation ends, even if Python retains the
object; identity remains readable. This boundary allows a callback invoked by
`Session.run()` to use permitted resources while the parent session has a
mutable Python borrow.

### Three patterns the boundary needs

**Declare the base class in Python, keep the logic in Rust.** `Provider`,
`Channel`, and `MemoryStore` are subclassed by user code, so they are Python
classes (`bindings/python/kerness/provider.py:99`,
`bindings/python/kerness/channel.py:21`,
`bindings/python/kerness/memory.py:31`). `Provider` holds a `_core` handle and
its methods forward `self` back down, so a subclass override wins by ordinary
Python method resolution rather than by anything the binding does. The other two
have no logic to forward — the four bundled channels and the three bundled stores
are crate types registered against their ABC at `channel.py:50` and
`memory.py:137`, so `isinstance` holds without inheritance.

**Own the pieces, build the borrowing value transiently.** `PromptAssembler<'a>`
and `AgentRunner<'a>` borrow their inputs, which no `#[pyclass]` can express.
The Python-facing class stores the pieces and constructs the Rust value inside
each call — `bindings/python/src/runtime.rs:266` and `:474`.

**Park channel exceptions.** A framework callback type cannot carry a `PyErr`,
so a raising Python channel's exception is stashed and re-raised at the pyclass
method boundary. See [channel.md](channel.md) for the full account. Tool and
event callbacks use the framework error conversion; the Rust runtime retains
those errors in its tool results or typed terminal outcome.

### Callback bindings and the memory filter

Python callbacks implement Rust traits, each bound where it is registered.
`Provider`, `Channel`, and `MemoryStore` are subclassed, so
the binding takes the instance — a store through `bind_memory_store`
(`bindings/python/src/memory.rs:165`), which passes a bundled store straight
through rather than round-tripping every call through the interpreter.
A tool handler is any callable and is bound by `Session.add_tool`. A context
source is a callable too, bound by `add_context`
(`bindings/python/src/session.rs:582`), called once per agent at the top of
`run()` rather than per turn. And a memory filter is bound by
`bind_memory_filter` (`:209`), which refuses a non-callable at construction
rather than at the first note an agent writes.

`PyFilter` (`:179`) is the one that fails closed rather than parking. A filter
that raises drops the note and logs a warning: the filter is a trust boundary
([memory.md](memory.md)), and a boundary that lets a note through because the
check crashed is not one. The parked-exception pattern would surface the error
to the caller, which is the right answer for a channel and the wrong one here —
by the time `run()` returned, the note would already be in the file.

An agent's `tools` (`bindings/python/src/types.rs:848`) and the access policy's
`allowed_hosts` (`bindings/python/src/access.rs:203`) are the mirror direction:
plain data, extracted at the boundary, validated by the crate.

Memory stores expose `maintenance_scopes()`, `maintain_scope(scope)`, and
`close_run()` through the ABC, bundled classes, and callback adapter. Rust
schedules and meters maintenance; cleanup forwards separately. Existing ABC,
`FileMemory`, and `CuratedMemory` subclasses that override `close()` retain
their cleanup callback. `SummarizingMemory.close()` is standalone paid
consolidation, so subclasses use `close_run()` for custom run cleanup and
`maintain_scope()` for paid maintenance; see [memory.md](memory.md).

## Interactions

- Wraps every module in `crates/kerness/src/`; each subsystem doc names its own
  pyclass.
- Exposes [run.md](run.md)'s runtime as owned handles and Rust-serialized values.
- Hands the exception hierarchy to [errors.md](errors.md) at bootstrap.
- Installs the patchable HTTP seam described in [provider.md](provider.md).
- Installs the console writer and the logger described in
  [channel.md](channel.md), and the console prompt described in
  [access.md](access.md).

## How to Test

```sh
cargo clippy --workspace --all-targets -- -D warnings # pass = exit 0
(cd bindings/python && ../../.venv/bin/maturin develop) # pass = installed workspace version
.venv/bin/python -m pytest bindings/python/tests -q   # pass = 0 failed
.venv/bin/python -m kerness.selfcheck                 # pass = all core checks passed
.venv/bin/python -m ruff check bindings/python        # pass = all checks passed
.venv/bin/python bindings/python/examples/host_control.py # pass = validated result, one operation
```

- The whole `bindings/python/tests/` suite exercises the Python surface only; a
  gap in the binding shows up as a test failure, not as a Rust compile error.
- `bindings/python/tests/test_packaging.py` asserts the installed package
  reports the root `Cargo.toml`'s `[workspace.package] version`, which is what
  catches a stale extension left in `site-packages` after a bump.

## Open Gaps / Roadmap

- M4: streaming and broader integrations remain deferred with the
  [core roadmap](../ARCHITECTURE.md#roadmap). New bindings should retain
  forwarding, error conversion, and handle-lifetime checks here; scheduling,
  approvals, recovery, and budget behavior remain Rust responsibilities.
- No `.pyi` stubs, so editors get no completion or type checking for `_core`.
  The shims re-export names without annotating them.
- `PyToolHandler` exposes a Rust closure as callable but not introspectable;
  `inspect.signature` on it gives the `__call__` signature, not the tool's schema.
- The wheel is built per platform. There is no `sdist`-only fallback for a
  platform without a prebuilt wheel beyond compiling the crate locally.
