---
eatmycode_version: "1.2.0"
---

# Channel

## Goal

Where a session's messages go. A channel receives every participant utterance
and every system note as it happens, so a harness can print to a console, append
to a JSONL log, write a transcript file, or push to a chat service without
waiting for the run to finish.

The framework's own diagnostics go through the same file: `logging.rs` is the
one-line-at-a-time sink that `debug`/`warning`/`error` write to, replaceable by
the caller so a Python process can route them into `logging`.

This module owns the `Channel` trait, the four bundled channels, the two
delivery seams, and the timestamp rendering. It does not decide *what* is
delivered or *when*: [session.md](session.md) and [run.md](run.md) write to it,
and [access.md](access.md) confines the paths it declares.

## Status

`done`. `cargo test -p kerness channel` passes 10 tests and
`.venv/bin/python -m pytest bindings/python/tests/test_channel.py -q` passes 7;
the session-level checks — a channel's paths confined at construction, command
verdicts reaching the channel and not stdout — pass inside
`bindings/python/tests/test_session.py`'s 122.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/channel.rs` | the `Channel` trait, four implementations, the `ConsoleWriter` seam, and the UTC timestamp arithmetic |
| `crates/kerness/src/logging.rs` | the `Logger` seam and the framework's `debug`/`warning`/`error` entry points |
| `bindings/python/src/channel.rs` | the four as pyclasses, `PyChannel` for a caller's own, `bind_channel`, and both delivery seams |
| `bindings/python/kerness/channel.py` | the `Channel` ABC Python subclasses, the virtual-subclass registration, and the re-exports |

## Language and Conventions

Two Rust crate modules, one PyO3 binding module, and one Python file that
declares an ABC. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- **Two of the four global seams live here.** `ConsoleWriter`
  (`crates/kerness/src/channel.rs:54`) and `Logger`
  (`crates/kerness/src/logging.rs:28`) are each an
  `OnceLock<RwLock<Arc<dyn Trait>>>` slot (`crates/kerness/src/channel.rs:73`,
  `crates/kerness/src/logging.rs:45`) with `expect("... lock poisoned")`.
  `write_console_line` (`crates/kerness/src/channel.rs:88`)
  clones the `Arc` out of the read lock before writing, so a writer that
  re-enters under the GIL cannot deadlock against a concurrent install.
- **The only `eprintln!` in the crate** is the default `StderrLogger`
  (`crates/kerness/src/logging.rs:39`, `:40`); the console default writes
  through a locked `stdout` handle, not `println!`, because `println!` panics
  on a closed pipe (`crates/kerness/src/channel.rs:67`).
- **No date-time dependency.** `Utc` (`crates/kerness/src/channel.rs:283`) is
  forty lines of civil-date arithmetic rendering the two formats the log needs
  (`compact` at `:315`, `iso8601` at `:323`), pinned byte-for-byte against
  CPython's output.
- **`Channel` is a Python ABC** (`bindings/python/kerness/channel.py:21`)
  because callers subclass it and the extension cannot declare one; the four
  bundled pyclasses are `frozen, subclass`
  (`bindings/python/src/channel.rs:176`, `:205`, `:233`, `:262`) and are
  registered as virtual subclasses at `channel.py:49`.
- **Tests.** Rust unit tests use `crate::testing::TempDir`, a `CaptureChannel`
  (`crates/kerness/src/channel.rs:363`), a `BrokenChannel` (`:401`), and a
  `CaptureWriter` (`:428`) installed over the process-wide slot and asserted
  with `contains` because a neighbouring test may write through it
  ([testing.md](testing.md)). The Python suite uses `capsys`, `caplog`, and
  `conftest.py`'s `CaptureChannel` (`bindings/python/tests/conftest.py:83`).

## Design and Invariants

### Decomposition and dependency direction

`channel` depends on `error`, `logging`, and `pyfmt`; `logging` depends on
nothing in the crate. Above them, the session writes through
`Shared.channel: Arc<dyn Channel>` — `record_and_emit` and `emit_system` in
`crates/kerness/src/session.rs:1623`, `:1639`, with the channel calls at
`:1631` and `:1641`; the owned run at
`crates/kerness/src/session/run.rs:627`, `:636`, `:1062` — and `log_command`
(`crates/kerness/src/session.rs:1988`) reports every command verdict as a
system notice. Diagnostics are raised from `agent_runtime`, `provider`,
`memory`, `session`, and `channel` itself, each through `logging::warning` or
`logging::error`.

### The four bundled channels live in the crate

They are the framework's behaviour, not the binding's: the console's prefix
template (`ConsoleChannel`, `crates/kerness/src/channel.rs:94`), the log's
`{role, sender, content, ts}` shape and its `session_<stamp>.jsonl` filename
(`LogChannel`, `:185`), the plain-text `[sender] message` line (`FileChannel`,
`:235`), and the fan-out's rule that one member's failure is logged and the
others still receive the message (`MultiChannel::fan_out`, `:147`). A Rust-only
harness gets all of it, and a second implementation in Python would be a second
set of answers to drift from the first.

What genuinely differs across the boundary is *delivery*, and that is two seams
rather than two implementations:

| Seam | Default | What the binding installs |
| --- | --- | --- |
| `ConsoleWriter` (`crates/kerness/src/channel.rs:54`) | this process's stdout | `builtins.print(..., flush=True)` (`bindings/python/src/channel.rs:309`) |
| `Logger` (`crates/kerness/src/logging.rs:28`) | warnings and errors to stderr | `logging.getLogger("kerness")` (`bindings/python/src/channel.rs:333`) |

Both exist because the two destinations are not the same one. `sys.stdout` is
not file descriptor 1, so a caller who replaced it, a notebook cell, and
pytest's `capsys` see a `print` and see nothing at all from a write to the
descriptor. `MultiChannel`'s failure report has the same problem against the
caller's `logging` handlers. The line is composed in the crate either way.

### Invariants a change must preserve

- **One member's failure never starves the others, and is never silent.**
  `fan_out` (`crates/kerness/src/channel.rs:147`) logs the failing channel by `type_name` and continues;
  `MultiChannel::send` always returns `Ok`. Tests:
  `a_failing_channel_does_not_starve_the_others` (`:418`) and
  `test_a_failing_channel_is_logged_and_does_not_starve_the_others`
  (`bindings/python/tests/test_channel.py:93`), the latter crossing the
  boundary twice — a Rust fan-out driving a Python member, reporting through
  the Rust logger into Python's `logging`.
- **`type_name` is required, not defaulted.** Its only reader is the failure
  log, and a placeholder there would name nothing the caller could fix
  (`crates/kerness/src/channel.rs:33`). Enforced by the trait signature.
- **A channel declares its files, and the workspace confines them.**
  `Channel::paths` (`crates/kerness/src/channel.rs:41`) defaults to nothing; `FileChannel` (`:260`) and
  `LogChannel` (`:229`) override it, and `MultiChannel::paths` (`:176`) is the
  union of its members' so wrapping a file channel does not hide it.
  `Session::new` checks every declared path before the first turn
  (`crates/kerness/src/session.rs:583`). Test:
  `test_a_channel_writing_outside_the_workspace_fails_at_construction`
  (`bindings/python/tests/test_session.py:1219`), bare and wrapped.
- **Paths are read once, at bind time.** `bind_channel`
  (`bindings/python/src/channel.rs:111`) calls `paths()` when the session is
  constructed, because `Channel::paths` cannot fail and a raising Python
  `paths()` needs somewhere to surface as an exception out of `Session(...)`. A
  duck-typed channel with no `paths` attribute gets the Rust default. The cost:
  a channel choosing its file later is not confined; the four bundled channels
  fix theirs in their constructor.
- **Binding a bundled channel is an exact-type shortcut.** `native_channel`
  (`bindings/python/src/channel.rs:150`) uses `downcast_exact`, so a Python subclass overriding `send` — the
  recording wrapper in
  `bindings/python/examples/texas_holdem/texas_holdem_three_players.py:19` is
  one — falls through to `PyChannel` and its override runs. Test:
  `test_a_subclass_that_wraps_send_is_not_bypassed`
  (`bindings/python/tests/test_channel.py:28`); with a plain `downcast` the
  base `send` is called and the subclass never sees the message.
- **A Python channel's exception is parked, not translated.** `Channel::send`
  returns `crate::error::Result`, and `Error` has no variant that can carry a
  Python class; `PyChannel::call` (`bindings/python/src/channel.rs:51`) keeps
  the first `PyErr` and lets the reduced framework error stop the run, and
  `Session.run`, `Session.start`, and `SessionRun.step` re-raise it
  (`bindings/python/src/session.rs:678`, `:632`;
  `bindings/python/src/run.rs:209`). Only a channel the caller wrote can park;
  `BoundChannel` (`bindings/python/src/channel.rs:100`) keeps the two cases
  apart. Inside a `MultiChannel` a
  member's parked exception is deliberately dropped, because re-raising it from
  `run()` would undo the degrade the fan-out just performed. No Python test
  drives a raising channel through `Session.run` directly; the invariant rests
  on `PyChannel::call` and the three drain sites.
- **`ConsoleWriter::write_line` is fallible.** A closed pipe is something the
  caller can act on and `Channel::send` already reports IO
  (`crates/kerness/src/channel.rs:60`). A diagnostic that cannot be delivered
  is dropped by the Python logger (`bindings/python/src/channel.rs:336`): it is
  already the report of something the framework survived.
- **Composition stays in the crate; only the final write moves.** Test:
  `console_lines_are_composed_here_and_delivered_through_the_writer`
  (`crates/kerness/src/channel.rs:440`).
- **Timestamps render as CPython renders them.** Tests:
  `timestamps_render_the_way_python_renders_them` (`crates/kerness/src/channel.rs:502`),
  `leap_days_and_century_boundaries_land_on_the_right_date` (`:519`).

### Extension points

- A new destination is a `Channel` impl in Rust or a `Channel` subclass in
  Python; override `paths` if it writes a file.
- A different console or log sink is a `ConsoleWriter` or `Logger` installed
  through `set_console_writer` (`crates/kerness/src/channel.rs:79`) or
  `set_logger` (`crates/kerness/src/logging.rs:51`).

## Key Types and Entry Points

- `crates/kerness/src/channel.rs:21` — `Channel` — `send(sender, message)`,
  `send_system(message)`, `type_name()`, `paths()`. The first two return
  `Result` because delivery is IO; `paths` at `:41` is the one defaulted method.
- `crates/kerness/src/channel.rs:54` — `ConsoleWriter` — `write_line(line)`;
  `set_console_writer(writer)` at `:79` installs a replacement, and
  `write_console_line` at `:88` is what `ConsoleChannel` calls.
- `crates/kerness/src/channel.rs:94` — `ConsoleChannel` — a `{sender}` template
  in front of each line, `[{sender}]` by default; system notices are
  `[System] ...`.
- `crates/kerness/src/channel.rs:138` — `MultiChannel` — fans out; `new` at
  `:143`, `fan_out` at `:147`, `paths` at `:176` unions its members'.
- `crates/kerness/src/channel.rs:185` — `LogChannel` — one JSON object per line
  in `session_<stamp>.jsonl`; `new` at `:191` creates the directory and claims
  the filename; `path()` at `:202`.
- `crates/kerness/src/channel.rs:235` — `FileChannel` — plain text, appended
  through `append_line` at `:269`; `new` at `:240` opens nothing until the
  first message.
- `crates/kerness/src/logging.rs:28` — `Logger` — `log(level, message)`;
  `set_logger` at `:51`; `debug`/`warning`/`error` are the crate-wide entry
  points.
- `bindings/python/src/channel.rs:29` — `PyChannel` — a Python object seen as a
  `Channel`; `type_name` and `paths` read once at construction, `parked()` at
  `:71` hands back the first exception a delivery raised.
- `bindings/python/src/channel.rs:111` — `bind_channel(object)` — `None` for
  `None`, the exact-type shortcut through `native_channel` (`:150`) for a
  bundled channel, otherwise a `PyChannel` with its paths read now; returns a
  `BoundChannel` (`:100`).
- `bindings/python/src/channel.rs:325` — `install_console_writer()` / `:355`
  `install_logger()` — installed at import by `bootstrap`.

## Interactions

- Written to by [session.md](session.md) on every turn and system note, and by
  [run.md](run.md) at each committed loop effect and command verdict. Shared
  state: `Shared.channel`. Contract: `send` may fail and the run stops; a
  `MultiChannel` absorbs member failure. Tested by
  `TestCommandLogGoesToTheChannel` (`bindings/python/tests/test_session.py:468`).
- Its declared paths are confined by [access.md](access.md)'s `check_path` at
  `Session::new`; the refusal names `The <type_name> destination`.
- Its parked exception is re-raised by [bindings.md](bindings.md)'s
  `Session.run`, `Session.start`, and `SessionRun.step`.
- Its two seams are installed at `bootstrap`, alongside the transport in
  [provider.md](provider.md) and the console prompt in [access.md](access.md).
- `logging` is called by [agent-runtime.md](agent-runtime.md),
  [provider.md](provider.md), [memory.md](memory.md), and
  [session.md](session.md) for anything the framework survived.
- `LogChannel` renders its `ts` through `pyfmt::json_dumps`
  ([utils.md](utils.md)), so the JSON line matches `json.dumps` byte for byte.

## How to Test

```sh
cargo test -p kerness channel                                       # pass = 10 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_channel.py -q # pass = 7 passed
.venv/bin/python -m pytest bindings/python/tests/test_session.py -q -k "Channel or channel" # pass = 0 failed
```

- `crates/kerness/src/channel.rs:418` — `a_failing_channel_does_not_starve_the_others`
  — a `BrokenChannel` (`:401`) inside a `MultiChannel`; the other channel still
  receives the message.
- `crates/kerness/src/channel.rs:440` —
  `console_lines_are_composed_here_and_delivered_through_the_writer` — the
  console seam at the layer that owns it: the prefix and the `[System]` label
  are the crate's, and only the final write moves.
- `crates/kerness/src/channel.rs:465` and `:480` — the file line and the log
  event shape; `:502` and `:519` — the timestamp rendering against CPython
  literals.
- `bindings/python/tests/test_channel.py:21` — `test_each_one_is_a_channel` —
  the four pyclasses against the Python base class, which they are registered
  with rather than inheriting.
- `bindings/python/tests/test_channel.py:28` —
  `test_a_subclass_that_wraps_send_is_not_bypassed` — the exact-type rule.
- `bindings/python/tests/test_channel.py:93` —
  `test_a_failing_channel_is_logged_and_does_not_starve_the_others` — a
  `BrokenChannel` raising `RuntimeError` inside a `MultiChannel`; the other
  members still receive the message and the failure reaches `caplog`.
- `bindings/python/tests/test_channel.py:47` — the console line reaches
  `capsys`, which is the whole claim of the installed writer.
- `bindings/python/tests/test_session.py:1219` —
  `test_a_channel_writing_outside_the_workspace_fails_at_construction` — a
  `FileChannel` pointed outside the workspace is refused, bare and wrapped in a
  `MultiChannel`, while a console channel and a confined file channel are not.
- `bindings/python/tests/test_session.py:493` —
  `test_both_verdicts_reach_the_channel_and_neither_reaches_stdout`.
- Gap: no test drives a raising Python channel through `Session.run` and
  asserts the original exception class comes back; the parked-exception path
  is covered only by the `MultiChannel` case, which deliberately drops it.

## Review and Refactor Guide

- **Changing a bundled channel's line or event shape** → `ConsoleChannel`,
  `LogChannel::write_event`, `FileChannel` (`crates/kerness/src/channel.rs:113`,
  `:206`, `:247`); tests `:440`, `:465`, `:480`, and the Python
  `bindings/python/tests/test_channel.py:47`, `:60`, `:118` assert literal text; a `LogChannel` field
  change is a change to every reader of the JSONL.
- **Changing the fan-out rule** → `fan_out` (`crates/kerness/src/channel.rs:147`); tests `:418` and
  `bindings/python/tests/test_channel.py:93`; the parked-exception drop inside `MultiChannel` in
  `bind_channel` must still hold.
- **Changing `Channel::paths` or when it is read** → the trait (`crates/kerness/src/channel.rs:41`),
  `bind_channel` (`bindings/python/src/channel.rs:111`), `Session::new`
  (`crates/kerness/src/session.rs:583`), and
  `bindings/python/tests/test_session.py:1219`; reading
  later than bind time loses the place a raising `paths()` can surface.
- **Changing a seam** → `ConsoleWriter`/`Logger` traits, their slots, and the
  binding installers (`bindings/python/src/channel.rs:325`, `:355`); the
  concurrency note in [testing.md](testing.md) applies to any test that installs
  a double.
- **Changing timestamp rendering** → `Utc` (`crates/kerness/src/channel.rs:283`) and the two CPython-pinned
  tests (`:502`, `:519`); do not add a date-time crate.
- **Safe extension points**: a new `Channel` impl; a new `ConsoleWriter` or
  `Logger`; overriding `paths` on a file-backed channel.
- **Forbidden coupling**: no `println!`/`eprintln!` outside `logging.rs`'s
  default; no direct `sys.stdout` from Rust — go through the writer; no
  non-exact `downcast` in `native_channel`; nothing in `channel.rs` may import
  `session`.
- **Compatibility checks**: `Channel.send`/`send_system` signatures and the
  optional `paths()` on the Python ABC; the four class names and constructor
  keywords (`prefix_format`, `filepath`, `log_dir`, `*channels`); the `kerness`
  logger name.

### Improvement candidates (proposals, not accepted work)

- Test the parked-exception path end to end: a Python channel raising a custom
  class from `send`, and `Session.run()` raising that exact class; success: a
  new case in `bindings/python/tests/test_session.py` that `pytest.raises` the
  caller's class.
- Make `LogChannel` claim its directory lazily so a directory outside the
  workspace is not created before the session refuses it; success:
  `bindings/python/tests/test_session.py:1219` extended to assert the escape directory does not
  exist after the refusal.

## Open Gaps / Roadmap

- Delivery is synchronous and inline. A slow channel slows the session; there is
  no buffering or drop policy.
- `LogChannel` selects one pathname per construction using a second-resolution
  stamp (`crates/kerness/src/channel.rs:195`), opens it for append on delivery
  (`:212`), and never rotates or prunes. Instances constructed in the same
  second under one directory share that pathname.
- `LogChannel` creates its log directory in its constructor
  (`crates/kerness/src/channel.rs:191`), so a directory outside the workspace
  exists by the time the session refuses it.
- Only the first exception from a channel is parked; later ones are discarded, so
  a `MultiChannel` with two broken members reports one.
