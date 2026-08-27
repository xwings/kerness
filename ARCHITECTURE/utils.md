# Utils

## Goal

The small shared primitives every other module reaches for: scanning a model's
reply for a keyword, an addressed agent, or a memory marker; retrying a fallible
call with backoff; and formatting values the way Python does, so that a value
crossing the boundary renders identically on both sides. Serves **M1**.

Nothing here is speculative — each function has more than one caller, which is
the bar for living in this module rather than beside its only user.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/utils.rs` | text scanning, session-end detection, retry |
| `crates/kerness/src/pyfmt.rs` | Python-compatible `repr`, `str`, `json.dumps`, truthiness |
| `bindings/python/kerness/utils.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/utils.rs:12` — `DEFAULT_TERMINATORS` — `CONSENSUS_REACHED`
  and `END_SESSION`.
- `crates/kerness/src/utils.rs:34` — `keyword_in_text(text, keyword)` — a
  hand-written boundary scan, not a regex: the `regex` crate has no lookaround,
  and the check is a word-boundary test that a single pass does exactly.
- `crates/kerness/src/utils.rs:64` — `parse_session_end(text, keywords)` — which
  terminator fired, if any.
- `crates/kerness/src/utils.rs:79` — `parse_orchestrator_call(text, agent_names)` —
  pulls `@Name` plus the message out of an orchestrator's reply; also a boundary
  scan.
- `crates/kerness/src/utils.rs:122` — `parse_memory_markers(text)` — splits a reply
  into the text to show and the memory entries to append.
- `crates/kerness/src/utils.rs:175` — `retry(...)` — attempts, backoff, and a
  predicate deciding what is retryable; the provider retry is built on it.
- `crates/kerness/src/pyfmt.rs:25` — `json_dumps_indent2(value)` — matches
  `json.dumps(..., indent=2)` byte for byte, which is what tool results and
  session files are compared against.
- `crates/kerness/src/pyfmt.rs:30` — `repr(value)` / `:62` `str(value)` — Python's
  two renderings, which differ for strings and for `True`/`None`.
- `crates/kerness/src/pyfmt.rs:70` — `truthy(value)` — Python's truthiness, not
  Rust's: an empty list, an empty string, and `0` are all false.

`pyfmt` exists because harness values are authored in YAML, rendered into
prompts, and asserted on from Python. A `Value` that renders as `true` on one
side and `True` on the other would make every such assertion a translation
exercise.

## Interactions

- `keyword_in_text` and `parse_orchestrator_call` are called by
  [loop.md](loop.md) on every orchestrator reply.
- `parse_memory_markers` feeds [memory.md](memory.md).
- `retry` backs [provider.md](provider.md)'s `chat_with_retries`.
- `pyfmt` is used wherever a value becomes prompt text or a tool result —
  [toolkit.md](toolkit.md), [prompting.md](prompting.md),
  [sessionfile.md](sessionfile.md).

## How to Test

```sh
cargo test -p kerness utils                                       # pass = 0 failed
cargo test -p kerness pyfmt                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_utils.py -q # pass = 0 failed
```

- `bindings/python/tests/test_utils.py` is table-driven: `TestParseOrchestratorCall` (`:13`),
  `TestParseSessionEnd` (`:30`), and `TestParseMemoryMarkers` (`:44`) each
  parametrise the boundary cases their scan exists for, and `TestRetry` (`:60`)
  covers first-attempt success, recovery, and exhaustion.
- The `pyfmt` Rust tests assert against literal strings taken from CPython, which
  is the only way the byte-for-byte claim can be checked.

## Open Gaps / Roadmap

- `pyfmt` covers the JSON value model only. There is nothing for a Python object
  that is not JSON-representable, and nothing needs it.
- `retry`'s backoff is fixed multiplicative with no jitter; see
  [provider.md](provider.md).
- User-supplied command patterns in [access.md](access.md) are compiled with
  `fancy-regex` rather than `regex`, because a caller's pattern may use
  lookaround. Framework-internal scanning stays lookaround-free by construction.
