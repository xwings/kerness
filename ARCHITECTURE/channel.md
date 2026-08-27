# Channel

## Goal

Where a session's messages go. A channel receives every participant utterance
and every system note as it happens, so a harness can print to a console, append
to a JSONL log, write a transcript file, or push to a chat service without
waiting for the run to finish. Serves **M1**.

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
  `type_name`; all three return `Result` because delivery is IO.
- `crates/kerness/src/channel.rs:37` — `ConsoleChannel` — a format string like
  `[{sender}]` in front of each line.
- `crates/kerness/src/channel.rs:80` — `MultiChannel` — fans out; one failing
  channel does not stop the others (`channel.py:62` documents the same rule on
  the Python side).
- `crates/kerness/src/channel.rs:118` — `LogChannel` — one JSON object per line in
  a dated file under a log directory.
- `crates/kerness/src/channel.rs:164` — `FileChannel` — plain text, appended.
- `bindings/python/src/channel.rs:23` — `PyChannel` — wraps a Python object;
  the `parked` field holds the first exception a delivery raised.
- `bindings/python/src/channel.rs:59` — `parked()` — hands that exception back
  so `Session.run` can re-raise it.
- `crates/kerness/src/logging.rs:51` — `set_logger(logger)` — installed at import
  by `bootstrap`.

### The parked exception

`Channel::send` returns `crate::error::Result`, and `Error` has no variant that
can carry a Python class. A Python channel that raises `RuntimeError` would
otherwise reach the caller as a `SessionError`, and `except RuntimeError` would
miss it. So `PyChannel::call` stashes the `PyErr`, lets the reduced framework
error stop the run, and `PySession::run` re-raises the original at the pyclass
boundary. The same pattern appears for callbacks in `runtime.rs`.

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

- The Rust tests include a `BrokenChannel` (`channel.rs:327`) inside a
  `MultiChannel` to prove the other channels still receive the message.
- `bindings/python/tests/test_channel.py:63` — `test_a_failing_channel_is_logged_and_does_not_starve_the_others` —
  a `BrokenChannel` raising `RuntimeError` inside a `MultiChannel`; the other
  members still receive the message and the failure is logged.

## Open Gaps / Roadmap

- Delivery is synchronous and inline. A slow channel slows the session; there is
  no buffering or drop policy.
- `LogChannel` writes one file per day per directory and never rotates or prunes.
- Only the first exception from a channel is parked; later ones are discarded, so
  a `MultiChannel` with two broken members reports one.
