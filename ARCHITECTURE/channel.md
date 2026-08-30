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
| `crates/kerness/src/channel.rs` | the `Channel` trait and four implementations |
| `crates/kerness/src/logging.rs` | the framework's own diagnostic sink |
| `bindings/python/src/channel.rs` | `PyChannel` — a Python object seen as a `Channel` |
| `bindings/python/kerness/channel.py` | the base class Python subclasses |

## Key Types and Entry Points

- `crates/kerness/src/channel.rs:21` — `Channel` — `send`, `send_system`,
  `type_name`, `paths`. The first two return `Result` because delivery is IO.
- `crates/kerness/src/channel.rs:41` — `Channel::paths` — the files this channel
  writes, so the session's workspace can confine them. Defaulted to nothing
  rather than required: a console prints and a caller's remote channel posts, so
  most channels have no file to declare, and overriding it is how a file-backed
  one opts into the same check the memory file and the session file go through.
- `crates/kerness/src/channel.rs:47` — `ConsoleChannel` — a format string like
  `[{sender}]` in front of each line.
- `crates/kerness/src/channel.rs:93` — `MultiChannel` — fans out; one failing
  channel does not stop the others (`channel.py:63` documents the same rule on
  the Python side). Its `paths` (`:131`) is the union of its members', so
  wrapping a file channel does not hide it from the workspace.
- `crates/kerness/src/channel.rs:140` — `LogChannel` — one JSON object per line in
  a dated file under a log directory.
- `crates/kerness/src/channel.rs:190` — `FileChannel` — plain text, appended.
- `bindings/python/src/channel.rs:24` — `PyChannel` — wraps a Python object;
  the `parked` field holds the first exception a delivery raised.
- `bindings/python/src/channel.rs:66` — `parked()` — hands that exception back
  so `Session.run` can re-raise it.
- `bindings/python/src/channel.rs:92` — `bind_channel(object)` — also where
  `paths()` is read, once. `Channel::paths` cannot fail, so a `paths()` that
  raises has nowhere to report from inside the trait; reading it here lets it
  reach the caller as an ordinary exception out of `Session(...)`. A channel
  that has no `paths` attribute at all — duck-typed, not inheriting the base
  class — gets the same empty answer the Rust default gives.
- `crates/kerness/src/logging.rs:51` — `set_logger(logger)` — installed at import
  by `bootstrap`.

### The parked exception

`Channel::send` returns `crate::error::Result`, and `Error` has no variant that
can carry a Python class. A Python channel that raises `RuntimeError` would
otherwise reach the caller as a `SessionError`, and `except RuntimeError` would
miss it. So `PyChannel::call` stashes the `PyErr`, lets the reduced framework
error stop the run, and `PySession::run` re-raises the original at the pyclass
boundary. The same pattern appears for callbacks in `runtime.rs`.

### Declared paths, read once

A channel's destinations are read at bind time, not per write, because the
session checks them in `Session::new` (`session.rs:502`) — before the first turn,
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

- The Rust tests include a `BrokenChannel` (`channel.rs:355`) inside a
  `MultiChannel` to prove the other channels still receive the message.
- `bindings/python/tests/test_channel.py:63` — `test_a_failing_channel_is_logged_and_does_not_starve_the_others` —
  a `BrokenChannel` raising `RuntimeError` inside a `MultiChannel`; the other
  members still receive the message and the failure is logged.
- `bindings/python/tests/test_session.py` —
  `test_a_channel_writing_outside_the_workspace_fails_at_construction` — a
  `FileChannel` pointed outside the workspace is refused, bare and wrapped in a
  `MultiChannel`, while a console channel and a confined file channel are not.

## Open Gaps / Roadmap

- Delivery is synchronous and inline. A slow channel slows the session; there is
  no buffering or drop policy.
- `LogChannel` writes one file per day per directory and never rotates or prunes.
- `LogChannel` creates its log directory in its constructor, so a directory
  outside the workspace exists by the time the session refuses it. Both
  surfaces behave the same way.
- Only the first exception from a channel is parked; later ones are discarded, so
  a `MultiChannel` with two broken members reports one.
