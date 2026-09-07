---
eatmycode_version: "1.2.0"
---

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
implemented here; the root's
[System Design](../ARCHITECTURE.md#system-design) states the rule and lists the
four seams that keep it true when a feature needs the interpreter.

This module owns the wheel's manifest and the boundary. It does not own any
behaviour a Python test observes: scheduling, approvals, recovery, budgets,
access, and prompt assembly stay in the crate, and each subsystem doc names its
own pyclass. The owned run and contextual tools are exposed through
`Session.start`, `SessionRun`, and `ToolContext`.

## Status

`done`. The blocking `run`, the owned-run and the contextual-tool APIs forward
to the Rust engine. `.venv/bin/python -m pytest bindings/python/tests -q` passes 502 tests
against the extension `maturin develop` installs;
`.venv/bin/python -m kerness.selfcheck` reports `OK: all core checks passed`;
`.venv/bin/ruff check bindings/python` reports `All checks passed!`; and
`bindings/python/examples/host_control.py` runs offline and exits 0.

## Code Structure

| File | Role |
| ---- | ---- |
| `bindings/python/src/lib.rs` | the module, `bootstrap`, and the class registry |
| `bindings/python/src/convert.rs` | JSON values and rendered chat messages ↔ Python objects |
| `bindings/python/src/errors.rs` | the exception map, both directions, and the `Raise`/`Catch` traits |
| `bindings/python/src/types.rs` | 21 pyclasses: tools, messages, agents, roles, harness specs; the dialect registry |
| `bindings/python/src/funcs.rs` | free functions and the framework constants |
| `bindings/python/src/runtime.rs` | `Conversation`, `ToolDispatcher`, `PromptAssembler`, `AgentRunner`, `LoopState`, `OrchestratorLoop`; the parked-exception helpers |
| `bindings/python/src/run.rs` | `SessionRun`, `RunControl`, `ToolContext`, and contextual/preflight/event callback translation |
| `bindings/python/src/session.rs` | `Session`, `SessionResult`; the keyword constructor, `PyFilter`, `PySource`, the consumed-session slot |
| `bindings/python/src/{provider,access,skill,channel,memory}.rs` | one boundary concern each; documented by their owning subsystem |
| `bindings/python/kerness/__init__.py` | bootstrap and the public surface |
| `bindings/python/kerness/<subsystem>.py` | re-export shims, one per crate module |
| `bindings/python/kerness/{provider,channel,memory}.py` | the three subclassable ABCs |
| `bindings/python/kerness/{exceptions,_enums,access}.py` | the exception hierarchy, `ToolDialect`, the `AccessPolicy` dataclass |
| `bindings/python/pyproject.toml` | the wheel's manifest; a Python build starts here |
| `bindings/python/Cargo.toml` | the `kerness-py` cdylib crate, a workspace member, `publish = false` |

## Language and Conventions

A Rust cdylib on `pyo3` 0.23 with `extension-module` and `abi3-py310`
(`bindings/python/Cargo.toml`), plus a Python 3.10+ package linted by `ruff`
with `select = ["E4", "E7", "E9", "F"]` (`bindings/python/pyproject.toml`). The
root's [Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- **`#[allow(clippy::too_many_arguments)]` is a binding idiom.** Thirteen of
  the workspace's fourteen `#[allow]` attributes on items sit in the binding:
  twelve pyo3 keyword constructors and methods —
  `bindings/python/src/session.rs:60`, `:349`, `:483`, `:632`;
  `bindings/python/src/types.rs:685`, `:1661`;
  `bindings/python/src/runtime.rs:417`, `:669`;
  `bindings/python/src/provider.rs:174`, `:221`, `:276`, `:323` — each with its
  Python signature spelled in the `#[pyo3(signature = (...))]` directly above,
  and the private `agent` helper that `add_agent` builds its record with
  (`bindings/python/src/session.rs:282`). Enforced by clippy `-D warnings`.
- **pyclass attribute shapes.** Every class is
  `#[pyclass(name = "...", module = "kerness._core")]`; value types add
  `frozen` and `get_all`; the four channels and three bundled stores add
  `frozen, subclass` (`bindings/python/src/channel.rs:176`,
  `bindings/python/src/memory.rs:187`); `PyToolContext` and `PyRunControl` are
  `frozen` (`bindings/python/src/run.rs:105`, `:172`); `PySessionRun` and `PySession` are not, so Python's
  mutable borrow refuses a reentrant `step()`. Rust structs are `Py<Name>`.
  Observed.
- **Error crossing is two traits, nothing hand-rolled.** `Raise` and `Catch`
  (`bindings/python/src/errors.rs:146`, `:157`) are used at every pyclass return
  and every call into Python; `to_py`/`from_py` (`:55`, `:92`) are the map.
- **GIL release.** `step` (`bindings/python/src/run.rs:209`), contextual
  `run_command` (`:166`), and the unpatched `http_post_json`
  (`bindings/python/src/provider.rs:768`) run under `allow_threads`; callbacks
  reacquire the GIL with `Python::with_gil`.
- **Python declarations are minimal and enforced.** `E402` is ignored only in
  `kerness/__init__.py`, because `bootstrap` must run before any shim imports
  from `_core`. Every public module carries `__all__` and every exported name
  resolves (`bindings/python/tests/test_packaging.py:74`, `:83`); every shim is
  in the self-check's core list (`bindings/python/tests/test_selfcheck.py:18`).
  No type checker is configured; `_core` ships no `.pyi`.
- **Tests.** pytest with `Test<Behaviour>` classes and `test_<sentence>`
  functions; providers are `conftest.py`'s `MockProvider` family, built with
  `retries=0, backoff_sec=0` (`bindings/python/tests/conftest.py:9`); built-in
  backends are reached by `@patch("kerness.provider.http_post_json")`
  (`bindings/python/tests/test_provider.py:95`).

## Design and Invariants

### Decomposition and dependency direction

`kerness-py` depends on `kerness`, `pyo3` and `serde_json` only; the crate has
zero `pyo3` references, and the Python package reaches the crate only through
`kerness._core` (`provider.py` and `toolschema.py` also import the package's
own `_enums` and `exceptions`). Inside the extension, `convert.rs` and `errors.rs` are leaves; `types.rs`
holds the value pyclasses every other module passes around; `session.rs`,
`run.rs`, and `runtime.rs` compose them. `bindings/python/src/lib.rs:52` is the `#[pymodule]`: `VERSION`, four
pyfunctions (`:54`), the explicit `add_class` list (`:62`) and
`funcs::register` (`:107`) are the extension's whole surface.

### Boot

`import kerness` runs `bindings/python/kerness/__init__.py:12`, which calls
`bootstrap(exceptions, dialect, assets_root)` (`bindings/python/src/lib.rs:36`).
The extension cannot declare three things itself and they are handed down: the
exception classes (structured constructors a `create_exception!` cannot
express), the `ToolDialect` enum (callers compare members with `is`, so it must
be a real `enum.Enum`, `bindings/python/kerness/_enums.py:14`), and the assets
root (only the package knows where pip put it). `bootstrap` then installs all
four seams — transport, console writer, logger, console prompt — which is why
an import is enough and no caller wires anything. `__version__` is
`env!("CARGO_PKG_VERSION")` (`bindings/python/src/funcs.rs:677`), re-exported at
`bindings/python/kerness/__init__.py:14`, so the number a caller reads is the one the binary was built
at.

### Three patterns the boundary needs

**Declare the base class in Python, keep the logic in Rust.** `Provider`,
`Channel`, and `MemoryStore` are subclassed by user code, so they are Python
classes (`bindings/python/kerness/provider.py:99`, `channel.py:21`,
`memory.py:31`). `Provider` holds a `_core` handle and its methods forward
`self` back down, so a subclass override wins by ordinary Python method
resolution rather than by anything the binding does. The other two have no
logic to forward — the four bundled channels and the three bundled stores are
crate types registered against their ABC at `channel.py:50` and `memory.py:137`,
so `isinstance` holds without inheritance. Binding takes the exact type only
(`native_channel`, `bindings/python/src/channel.rs:150`; `bind_memory_store`,
`bindings/python/src/memory.rs:165`): a subclass overriding `send` or `read` is a caller's object
that happens to inherit, and the shortcut past it would call the base the
subclass exists to wrap. Test:
`test_a_subclass_that_wraps_send_is_not_bypassed`
(`bindings/python/tests/test_channel.py:28`).

**Own the pieces, build the borrowing value transiently.** `PromptAssembler<'a>`
and `AgentRunner<'a>` borrow their inputs, which no `#[pyclass]` can express.
The Python-facing class stores the pieces and constructs the Rust value inside
each call (`bindings/python/src/runtime.rs:266` and `:474`); the cost is one
construction per turn.

**Park the exception.** A framework callback type cannot carry a `PyErr`
(`Fn(&Agent) -> String` has nowhere to put one), so a raising Python callable is
parked and re-raised at the pyclass boundary rather than read as an empty
answer: `park`/`unpark` in `bindings/python/src/runtime.rs:47`, `:57`, and
`PyChannel::parked` (`bindings/python/src/channel.rs:71`), drained by `Session.run`, `Session.start`,
and `SessionRun.step`. Tool, context-source, preflight, and event-sink callbacks
instead use `Catch`, and the Rust runtime retains the converted error in its
tool result or typed terminal outcome. See [channel.md](channel.md) and
[errors.md](errors.md).

### Callback bindings and the memory filter

Python callbacks implement Rust traits, each bound where it is registered. A
tool handler is any callable and is bound by `Session.add_tool` as `PyHandler`
(`bindings/python/src/session.rs:159`); a context source by `add_context` as
`PySource` (`:235`, called once per agent at the top of the run); a memory
filter by `bind_memory_filter` (`:209`), which refuses a non-callable at
construction rather than at the first note an agent writes.

`PyFilter` (`bindings/python/src/session.rs:179`) is the one callback that fails closed rather than parking.
A filter that raises drops the note and logs a warning (`:190`): the filter is a
trust boundary ([memory.md](memory.md)), and a boundary that lets a note through
because the check crashed is not one. Test:
`test_the_filter_runs_before_the_store_sees_a_note`
(`bindings/python/tests/test_session.py:2586`).

An agent's `tools` (`bindings/python/src/types.rs:848`) and the access policy's
`allowed_hosts` (`bindings/python/src/access.rs:203`) are the mirror direction:
plain data, extracted at the boundary, validated by the crate. The Python suite
proves such a value crossed with one case and leaves its semantics to the crate
tests ([testing.md](testing.md)).

### Owned execution and contextual tools

`Session.start(*, mode="automatic", approvals="external", budget=None,
pricing=None, event_sink=None, result_validation="strict", binding_version="")`
(`bindings/python/src/session.rs:632`) transfers the Rust session into a
`SessionRun`. The Python session retains an empty slot afterward; further
configuration or execution through that object raises `SessionError` naming the
existing run handle (`prepared`, `:263`). Invalid Python argument conversion is
rejected before transfer; a Rust preparation failure consumes the configuration,
matching the Rust API. `Session.run()` (`:678`) blocks until the run ends and
leaves the session available afterward.

`SessionRun.step(input=None)` (`bindings/python/src/run.rs:189`) deserialises a
JSON-compatible dictionary into Rust `RunInput`; `None` is `continue`. Its
returned dictionary is the serialised Rust `StepOutcome`. Inputs use a `kind`
field (`select_agent`, `user_message`, `approve`, `reconcile`, `finish`);
outcomes use `status` (`progress`, `waiting`, `finished`). The engine validates
and executes every transition. `outcome()`, `usage()`, and `drain_events()` use
the same JSON conversion; `checkpoint()` (`:222`) forwards; `control()` (`:216`)
hands out an independent cancellation handle that does not borrow the run, so
cancellation is available to another Python thread without a framework thread.

An optional `event_sink(event)` receives each Rust event as a dictionary
(`PyEventSink`, `bindings/python/src/run.rs:88`). Events observe the run; decisions enter through `step` or
the control handle. The Rust runtime stops on a sink error and does not replay
a completed tool to redeliver an event. `budget`, `pricing`, and
`binding_version` are forwarded to Rust; no scheduling, pricing, access,
recovery, or budget policy lives in Python.

`Session.add_tool_spec(spec)` preserves `ToolSpec.takes_actor`.
`add_contextual_tool(name, description, parameters, handler, *,
preflight=None)` (`bindings/python/src/session.rs:556`) sits beside `add_tool`: a plain
handler receives `(arguments)`, and a contextual handler receives
`(arguments, context)` (`PyContextHandler::call`, `bindings/python/src/run.rs:70`).
An optional preflight receives `(arguments, identity_dict)` and returns `None`
or the Rust action shape — `{"kind": "confirm", "description": "..."}` or
`{"kind": "command", "command": "...", "cwd": null}` (`:46`). It must be free
of side effects, and an arbitrary synchronous callback cannot suspend mid-stack.

`ToolContext` (`bindings/python/src/run.rs:105`) owns a clone of the Rust capability handle, with
read-only `actor`, `run_id`, numeric `turn_id`, and `call_id` properties. The
call ID is the engine's correlation ID, not the provider's native tool-call ID.
Its file, directory, command, and memory methods use that actor's policy
without borrowing `PySession`. Capabilities expire when the invocation ends,
even if Python retains the object; identity remains readable. Test:
`test_inputs_results_events_and_handles_cross_the_boundary`
(`bindings/python/tests/test_session.py:403`).

### Invariants a change must preserve

- Every `_core` class is registered in `bindings/python/src/lib.rs:52`'s list; a class missing
  there is invisible from Python. Enforced by
  `test_every_exported_name_resolves` (`bindings/python/tests/test_packaging.py:83`).
- Dictionary key order survives a round trip: `serde_json` is built with
  `preserve_order`, and `value_from_py`/`value_to_py`
  (`bindings/python/src/convert.rs:59`, `:15`) walk in order. Observed; no
  dedicated test.
- The framework's constants are named once. `funcs::register`
  (`bindings/python/src/funcs.rs:672`) re-exports every constant from the crate
  values; `test_the_constants_carry_the_frameworks_values`
  (`bindings/python/tests/test_provider.py:841`) and the root's constants table
  hold them.
- The wheel carries the package and distribution metadata. `pyproject.toml`'s `module-name` is
  `kerness._core` and `python-source = "."`, so `tests/` and `examples/` beside
  the package do not ship; `exclude` drops stray `__pycache__` so a local build
  cannot embed interpreter-specific bytecode in an `abi3` wheel. The `LICENSE`
  and `README.md` symlinks in `bindings/python/` are load-bearing because
  `readme` and `license-files` reject a `..` path
  ([testing.md](testing.md)).
- `kerness.__version__` equals the workspace version;
  `test_the_package_reports_the_workspace_version`
  (`bindings/python/tests/test_packaging.py:35`) detects a version mismatch.
  It cannot detect source changes within the same version.

## Key Types and Entry Points

- `bindings/python/src/lib.rs:36` — `bootstrap(exceptions, dialect,
  assets_root)` — called once from `bindings/python/kerness/__init__.py:12`;
  registers the exception classes and the dialect enum, sets the assets root,
  installs the four seams.
- `bindings/python/src/lib.rs:52` — `_core(module)` — the `#[pymodule]`:
  `VERSION`, the four pyfunctions at `:54`, the explicit `add_class` list at
  `:62`, then `funcs::register` at `:107`.
- `bindings/python/src/convert.rs:59` — `value_from_py(object)` — Python to
  `Value`, order preserved; `value_to_py` at `:15` is the inverse;
  `chat_message_to_py` at `:113` owns the shared `{role, content}` dictionary
  shape.
- `bindings/python/src/errors.rs:146` — `Raise<T>` / `:157` `Catch<T>` — the two
  extension traits that turn `Result<T>` into `PyResult<T>` and back; `to_py`
  at `:55` builds the right class with the right arguments, `from_py` at `:92`
  folds an unrecognised Python exception into `Error::Session`.
- `bindings/python/src/types.rs:40` — `register_dialect(class)` and the
  `dialect_to_py`/`dialect_from_py` pair — the enum member crossing by value.
- `bindings/python/src/types.rs:166` — `PyToolHandler` — a Rust closure seen
  from Python as an ordinary callable, so `spec.handler(...)` works for the
  built-in and `Skill` tools.
- `bindings/python/src/funcs.rs:672` — `register(module)` — every free
  function and every framework constant, `__version__` first at `:677`.
- `bindings/python/src/session.rs:251` — `PySession` — the keyword constructor
  assembles `SessionConfig`; `add_*` return the same object so registration
  chains; `start` at `:632` consumes the slot, `run` at `:678` does not.
- `bindings/python/src/run.rs:189` — `PySessionRun` — `step` under
  `allow_threads`, `control`, `checkpoint`, `drain_events`, `outcome`, `usage`;
  `PyToolContext` at `:105` carries invocation capabilities; `PyContextHandler`
  at `:40` and `PyEventSink` at `:88` translate the callbacks.
- `bindings/python/src/runtime.rs:47` — `park` / `:57` `unpark` — the
  parked-exception helpers behind `PromptAssembler` and `AgentRunner`;
  `PyChannel::parked` (`bindings/python/src/channel.rs:71`) is the channel's
  copy of the same idea.

## Interactions

- Wraps every module in `crates/kerness/src/`; each subsystem doc names its own
  pyclass and its binding tests.
- Exposes [run.md](run.md)'s runtime as owned handles and Rust-serialised
  dictionaries; the `kind`/`status` tags are that module's serde contract.
- Exposes [session.md](session.md)'s configuration as keyword arguments; the
  consumed-session slot is the Python face of `Session::start` taking `self`
  by value.
- Hands the exception hierarchy to [errors.md](errors.md) at bootstrap; the
  map runs both ways because a Python provider raises into Rust code that reads
  the status code.
- Installs the patchable HTTP seam described in [provider.md](provider.md)
  (`install_transport`, `bindings/python/src/provider.rs:747`), the console
  writer and logger described in [channel.md](channel.md), and the console
  prompt described in [access.md](access.md).
- Binds Python stores through [memory.md](memory.md)'s `bind_memory_store` and
  channels through [channel.md](channel.md)'s `bind_channel`; both take the
  exact type shortcut for bundled objects.
- Ships the assets [gameplan.md](gameplan.md), [role.md](role.md),
  [persona.md](persona.md), and [skills.md](skills.md) load; byte-equality with
  the crate's copy is asserted by
  `test_the_crate_and_the_package_ship_the_same_assets`
  (`bindings/python/tests/test_packaging.py:42`).
- Is proved installable by [selfcheck.md](selfcheck.md) and gated by
  [testing.md](testing.md).

## How to Test

```sh
cargo clippy --workspace --all-targets -- -D warnings           # pass = exit 0
(cd bindings/python && ../../.venv/bin/maturin develop)         # pass = "Installed kerness-<workspace version>"
.venv/bin/python -m pytest bindings/python/tests -q             # pass = 502 passed
.venv/bin/python -m kerness.selfcheck                           # pass = "OK: all core checks passed", exit 0
.venv/bin/ruff check bindings/python                            # pass = "All checks passed!"
.venv/bin/python bindings/python/examples/host_control.py       # pass = exit 0, validated result
```

- Rebuild with `maturin develop` after any Rust change; the Python suite tests
  the installed extension, and `bindings/python/tests/test_packaging.py:35`
  detects version mismatches only; rebuilds within a version remain necessary.
- `bindings/python/tests/test_packaging.py:74` and `:83` — every public module
  declares `__all__` and every name in it resolves, which is what catches a
  renamed Rust symbol behind a shim.
- `bindings/python/tests/test_session.py:403` —
  `test_inputs_results_events_and_handles_cross_the_boundary` — inputs,
  outcomes, events, `ToolContext`, and `RunControl` cross once; Rust owns the
  loop.
- `bindings/python/tests/test_session.py:2492` —
  `test_a_python_store_is_opened_read_written_and_closed` — a `MemoryStore`
  subclass driven by Rust through every lifecycle method.
- `bindings/python/tests/test_channel.py:28` — the exact-type rule; with a
  plain `downcast` this fails because the shortcut calls the base `send`.
- `bindings/python/tests/test_provider.py:365` —
  `test_the_budget_is_spent_only_on_failure_and_then_reported` — a Python
  subclass's `chat` is what the supplied retry body calls.
- `bindings/python/tests/test_examples.py:132` — every `kerness.X` a Python
  example reaches for still exists, by AST walk, since the examples need keys.
- Gap: `PyToolHandler` is callable but not introspectable
  (`inspect.signature` gives `__call__`, not the tool's schema); no test asserts
  either way. Dictionary order preservation across `convert.rs` has no
  dedicated test.

## Review and Refactor Guide

- **Adding a pyclass** → `types.rs` or the owning boundary module, then the
  `add_class` list at `bindings/python/src/lib.rs:52`, the shim's import and
  `__all__`, and (for a new shim) `_CORE_MODULES` in
  `bindings/python/kerness/selfcheck.py`; `bindings/python/tests/test_packaging.py:83` and
  `bindings/python/tests/test_selfcheck.py:18` fail until all three agree.
- **Adding a constant** → declare it in the crate, add one `module.add` in
  `funcs::register` (`bindings/python/src/funcs.rs:672`), re-export it in the
  shim, and extend `test_the_constants_carry_the_frameworks_values`
  (`bindings/python/tests/test_provider.py:841`) or the root's table; never
  spell the value in Python.
- **Changing `Session.start` or `SessionRun.step`** → `bindings/python/src/session.rs:632`
  and `bindings/python/src/run.rs:189`; the `kind`/`status` tags are [run.md](run.md)'s serde
  contract, so a variant rename lands here as a public change;
  `bindings/python/tests/test_session.py:403` and the README's host-control
  section.
- **Changing a callback signature** → the trait impl in
  `bindings/python/src/run.rs:40` or `:88`, or `bindings/python/src/session.rs:159`,
  `:179`, `:235`; `require_callable`
  (`bindings/python/src/run.rs:32`) is the construction-time check to reuse.
- **Changing error crossing** → `errors.rs` only; every other file uses
  `Raise`/`Catch`. A new `Error` variant needs a class in
  `bindings/python/kerness/exceptions.py`, a `Classes` field, and arms in both
  `to_py` and `from_py`; see [errors.md](errors.md).
- **Safe extension points**: a new `#[pyfunction]` in `funcs.rs`; a new
  `with_*` forwarded builder; a new seam installed in `bootstrap`.
- **Forbidden coupling**: no logic in the Python package beyond ABCs,
  dataclasses, enums, exceptions, `inspect`, and `pydantic`; no `pyo3` in
  `crates/`; no direct `sys.stdout`/`stderr` writes from Rust — go through the
  seams; no non-exact `downcast` when binding a bundled channel or store.
- **Compatibility checks**: keyword names and order in every
  `#[pyo3(signature = ...)]`; the `_core` class names; `kerness.__all__`
  (`bindings/python/kerness/__init__.py:80`); the wheel tag
  `cp310-abi3`; `requires-python = ">=3.10"`.

### Improvement candidates (proposals, not accepted work)

- Ship `.pyi` stubs for `_core` so editors get completion and a type checker
  can run over `bindings/python`; success: `maturin develop` installs a
  `_core.pyi` and a stub-consistency test compares it with `dir(_core)`.
- Give `PyToolHandler` a `__signature__` derived from the tool's parameter
  schema; success: `inspect.signature(spec.handler)` names the schema's
  properties in a new `test_toolkit.py` case.

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
