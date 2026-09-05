# Channel

## Goal

Where a session's messages go. A channel receives every participant utterance
and every system note as it happens, so a harness can print to a console, append
to a JSONL log, write a transcript file, or push to a chat service without
waiting for the run to finish.

The framework's own diagnostics go through the same file: `logging.rs` is the
one-line-at-a-time sink that `debug`/`warning`/`error` write to, replaceable by
the caller so a Python process can route them into `logging`.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/channel.rs` | the `Channel` trait, four implementations, and the console seam |
| `crates/kerness/src/logging.rs` | the framework's own diagnostic sink |
| `bindings/python/src/channel.rs` | the four as pyclasses, `PyChannel` for a caller's own, and both delivery seams |
| `bindings/python/kerness/channel.py` | the base class Python subclasses, and the re-exports |

## Key Types and Entry Points

- `crates/kerness/src/channel.rs:21` — `Channel` — `send`, `send_system`,
  `type_name`, `paths`. The first two return `Result` because delivery is IO.
- `crates/kerness/src/channel.rs:41` — `Channel::paths` — the files this channel
  writes, so the session's workspace can confine them. Defaulted to nothing
  rather than required: a console prints and a caller's remote channel posts, so
  most channels have no file to declare, and overriding it is how a file-backed
  one opts into the same check the memory file and the session file go through.
- `crates/kerness/src/channel.rs:54` — `ConsoleWriter` — where a console line is
  actually written; `:79` `set_console_writer(writer)` installs a replacement.
- `crates/kerness/src/channel.rs:94` — `ConsoleChannel` — a format string like
  `[{sender}]` in front of each line.
- `crates/kerness/src/channel.rs:138` — `MultiChannel` — fans out; one failing
  channel does not stop the others. Its `paths` (`:176`) is the union of its
  members', so wrapping a file channel does not hide it from the workspace.
- `crates/kerness/src/channel.rs:185` — `LogChannel` — one JSON object per line in
  a dated file under a log directory.
- `crates/kerness/src/channel.rs:235` — `FileChannel` — plain text, appended.
- `bindings/python/src/channel.rs:176`, `:205`, `:233`, `:262` — the four as
  pyclasses over the crate's implementations.
- `bindings/python/src/channel.rs:309` — `PyConsoleWriter`, and `:325`
  `install_console_writer()` — installed at import by `bootstrap`.
- `bindings/python/src/channel.rs:29` — `PyChannel` — wraps a Python object;
  the `parked` field holds the first exception a delivery raised.
- `bindings/python/src/channel.rs:71` — `parked()` — hands that exception back
  so `Session.run` can re-raise it.
- `bindings/python/src/channel.rs:100` — `BoundChannel` — what the session writes
  to, plus the `PyChannel` behind it when there is one. Only a channel the caller
  wrote can park an exception; a bundled one is Rust and reports through `Result`.
- `bindings/python/src/channel.rs:111` — `bind_channel(object)` — also where
  `paths()` is read, once. `Channel::paths` cannot fail, so a `paths()` that
  raises has nowhere to report from inside the trait; reading it here lets it
  reach the caller as an ordinary exception out of `Session(...)`. A channel
  that has no `paths` attribute at all — duck-typed, not inheriting the base
  class — gets the same empty answer the Rust default gives.
- `bindings/python/src/channel.rs:150` — `native_channel(object)` — the shortcut
  past `PyChannel` for a bundled channel, on the *exact* type only.
- `bindings/python/kerness/channel.py:21` — `Channel` — the abstract base class,
  which stays Python because the extension cannot declare one; `:49` registers
  the four concrete channels as virtual subclasses so `isinstance` holds.
- `crates/kerness/src/logging.rs:51` — `set_logger(logger)` — installed at import
  by `bootstrap`.

### The four bundled channels live in the crate

They are the framework's behaviour, not the binding's: the console's prefix, the
log's `{role, sender, content, ts}` shape and its `20260827T134501Z` filename,
the plain-text line, and the fan-out's rule that one member's failure is logged
and the others still receive the message. A Rust-only harness gets all of it,
and a second implementation in Python would be a second set of answers to drift
from the first.

What genuinely differs across the boundary is *delivery*, and that is two seams
rather than two implementations:

| Seam | Default | What the binding installs |
| --- | --- | --- |
| `ConsoleWriter` (`channel.rs:54`) | this process's stdout | Python's `print` |
| `Logger` (`logging.rs:28`) | stderr | `logging.getLogger("kerness")` |

Both exist because the two destinations are not the same one. `sys.stdout` is
not file descriptor 1, so a caller who replaced it, a notebook cell, and
pytest's `capsys` see a `print` and see nothing at all from a write to the
descriptor. `MultiChannel`'s failure report has the same problem against the
caller's `logging` handlers. The line is composed in the crate either way.

`ConsoleWriter::write_line` returns `Result` rather than swallowing, because
`Channel::send` already reports IO failure and a closed pipe is something the
caller can act on — `println!` would panic on it instead of saying so.

### Binding a bundled channel

`bind_channel` recognises the four pyclasses and hands the session the
`Arc<dyn Channel>` inside, rather than wrapping them in `PyChannel` for a GIL
acquisition and a Python dispatch per line that arrive back at the Rust call
they started from.

The match is on the exact type (`downcast_exact`). The four are `subclass`, and
a subclass overriding `send` — the recording wrapper in
`examples/texas_holdem/texas_holdem_three_players.py:19` is one — is a caller's
channel that happens to inherit. Taking the shortcut past it would call the base
implementation it exists to wrap, silently. Falling through to `PyChannel` calls
the override, whose `super().send` reaches the same Rust code one level down.

Inside a `MultiChannel`, a member's `parked()` is deliberately dropped:
re-raising it from `run()` would undo the degrade the fan-out just performed.

### The parked exception

`Channel::send` returns `crate::error::Result`, and `Error` has no variant that
can carry a Python class. A Python channel that raises `RuntimeError` would
otherwise reach the caller as a `SessionError`, and `except RuntimeError` would
miss it. So `PyChannel::call` stashes the `PyErr`, lets the reduced framework
error stop the run, and `PySession::run` re-raises the original at the pyclass
boundary. The same pattern appears for callbacks in `runtime.rs`.

### Declared paths, read once

A channel's destinations are read at bind time, not per write, because the
session checks them in `Session::new` (`session.rs:549`) — before the first turn,
so a misplaced log fails at construction rather than mid-run. The cost is that a
channel choosing its file later is not confined by the workspace; the four bundled
channels all fix theirs in their constructor.

## Interactions

- Written to by [session.md](session.md) on every turn and system note.
- Its exception is re-raised by [session.md](session.md)'s `run`.
- `MultiChannel` composes the others and is what a harness with both a console
  and a log file uses.

## How to Test

```sh
cargo test -p kerness channel                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_channel.py -q # pass = 0 failed
```

- The Rust tests include a `BrokenChannel` (`channel.rs:401`) inside a
  `MultiChannel` to prove the other channels still receive the message.
- `crates/kerness/src/channel.rs:440` —
  `console_lines_are_composed_here_and_delivered_through_the_writer` — the
  console seam at the layer that owns it: the prefix and the `[System]` label
  are the crate's, and only the final write moves.
- `bindings/python/tests/test_channel.py:21` — `test_each_one_is_a_channel` —
  the four pyclasses against the Python base class, which they are registered
  with rather than inheriting.
- `bindings/python/tests/test_channel.py:28` —
  `test_a_subclass_that_wraps_send_is_not_bypassed` — the exact-type rule; with
  a plain `downcast` this fails, because the shortcut calls the base `send` and
  the subclass never sees the message.
- `bindings/python/tests/test_channel.py:93` — `test_a_failing_channel_is_logged_and_does_not_starve_the_others` —
  a `BrokenChannel` raising `RuntimeError` inside a `MultiChannel`; the other
  members still receive the message and the failure is logged. Both halves cross
  the boundary twice: a Rust fan-out driving a Python member, reporting through
  the Rust logger into Python's `logging`.
- `bindings/python/tests/test_session.py` —
  `test_a_channel_writing_outside_the_workspace_fails_at_construction` — a
  `FileChannel` pointed outside the workspace is refused, bare and wrapped in a
  `MultiChannel`, while a console channel and a confined file channel are not.

## Open Gaps / Roadmap

- Delivery is synchronous and inline. A slow channel slows the session; there is
  no buffering or drop policy.
- `LogChannel` writes one file per day per directory and never rotates or prunes.
- `LogChannel` creates its log directory in its constructor, so a directory
  outside the workspace exists by the time the session refuses it.
- Only the first exception from a channel is parked; later ones are discarded, so
  a `MultiChannel` with two broken members reports one.
