---
eatmycode_version: "1.2.0"
---

# Utils

## Goal

The small shared primitives every other module reaches for: scanning a model's
reply for a keyword, an addressed agent, or a memory marker; retrying a fallible
call with backoff; and formatting values the way Python does, so that a value
crossing the boundary renders identically on both sides.

Nothing here is speculative — each function has more than one caller, which is
the bar for living in this module rather than beside its only user. It owns no
policy: which terminators a harness declares, how a retry is configured, and
what a rendered value is used for are all the caller's.

## Status

`done` — `cargo test -p kerness utils` passes 5 tests, `cargo test -p kerness
pyfmt` passes 5, and `bindings/python/tests/test_utils.py` passes 22.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/utils.rs` | text scanning, session-end detection, retry |
| `crates/kerness/src/pyfmt.rs` | Python-compatible `repr`, `str`, `json.dumps`, truthiness |
| `bindings/python/src/funcs.rs:48` | the five `utils` pyfunctions and `DEFAULT_TERMINATORS` (`:679`) |
| `bindings/python/kerness/utils.py` | re-export shim; also re-exports `http_post_json`, which [provider.md](provider.md) owns |

## Language and Conventions

Two Rust crate modules, five pyfunctions, one Python shim; the root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- Neither module imports anything else from the crate
  (`crates/kerness/src/utils.rs:1`, `crates/kerness/src/pyfmt.rs:1`); `pyfmt`
  is imported by nineteen crate modules and four binding modules. That is the
  bottom of the dependency graph, and the reason nothing here may grow a
  dependency upward.
- No function here returns `crate::error::Result`; `retry` is generic over the
  caller's error type (`crates/kerness/src/utils.rs:179`) and everything else
  is pure.
- The scans are hand-written byte or char loops, not regexes: `keyword_in_text`
  (`crates/kerness/src/utils.rs:34`) and `find_mention` (`:91`) need a word boundary and the `regex` crate
  has no lookaround; `parse_memory_markers` (`:122`) keeps line endings through
  `split_lines_keepends` (`:143`). Observed convention, stated in the module
  doc.
- `pyfmt`'s `expect` messages name the impossibility
  (`"writing to a Vec cannot fail"`, `crates/kerness/src/pyfmt.rs:21`), matching
  the crate idiom for infallible-by-construction calls.
- The `retry` pyfunction releases the GIL for its sleeps
  (`bindings/python/src/funcs.rs:90`) and reacquires it to call the Python
  callable; `parse_session_end` defaults `keywords=None` to
  `DEFAULT_TERMINATORS` (`:67`).
- Python tests are table-driven with `pytest.mark.parametrize`
  (`bindings/python/tests/test_utils.py:13`, `:30`, `:44`); the `pyfmt` Rust
  tests assert against literal strings taken from CPython.

## Design and Invariants

**Boundary-aware matching.** A protocol token is delimited by `[A-Za-z0-9_]` on
both sides, so `END_SESSION` in prose does not fire on `END_SESSIONS`
(`keyword_in_text`, `crates/kerness/src/utils.rs:34`). Matching is
case-insensitive and byte-wise, which is correct on UTF-8 without decoding
because a continuation byte is never in the class. An empty keyword never
matches, or a harness that declared one would end on every reply. Test:
`keyword_matches_only_as_a_standalone_token` (`:215`).

**Terminator priority is declaration order.** `parse_session_end`
(`crates/kerness/src/utils.rs:64`) tries
the harness's `terminate_on` list in order and returns the first present, so
when a reply names two the gameplan author decides which is recorded, not word
order. Test: `terminator_priority_follows_declaration_order` (`:225`).

**Routing follows the roster, not the leftmost mention.**
`parse_orchestrator_call` (`crates/kerness/src/utils.rs:79`) tries agent names in the order given and the
first one present anywhere wins; `find_mention` (`:91`) mirrors `@{name}\b`
with a Unicode-aware word test, so `@Alicia` does not address `Alice`. The
instruction is everything after the mention with leading `,`, `:` and spaces
removed. Test: `mention_parsing_strips_leading_punctuation` (`:235`).

**Markers are instructions, not transcript.** `parse_memory_markers`
(`crates/kerness/src/utils.rs:122`)
removes every line whose stripped form starts with `@MEMORY:` (case-insensitive)
and returns the notes separately; an empty marker records nothing. Both the
session's memory path (`crates/kerness/src/session.rs:1612`) and the run's
delivery fallback (`crates/kerness/src/session/run.rs:1054`) strip them, so a
marker never reaches a channel even when there is nowhere to store it. Test:
`memory_markers_are_stripped_and_collected` (`crates/kerness/src/utils.rs:246`).

**Retry is `retries + 1` attempts with a linear or fixed wait.** `retry`
(`crates/kerness/src/utils.rs:179`) sleeps `backoff_sec * attempt` between attempts unless `interval_sec`
pins a fixed wait, and returns the last error when every attempt fails; every
returned error is retried, with no classification. Test:
`retry_returns_the_last_error_after_exhausting_attempts` (`:260`); the Python
`TestRetry` (`bindings/python/tests/test_utils.py:60`) covers first-attempt
success, recovery, and exhaustion.

**Rendered values match CPython byte for byte.** `json_dumps` reproduces
`json.dumps(value, ensure_ascii=True)`: `", "` and `": "` separators and
`\uXXXX` escapes for every non-ASCII character, surrogate pairs included, via a
custom `serde_json` `Formatter` (`PythonFormatter`,
`crates/kerness/src/pyfmt.rs:109`). `repr` renders `None`/`True`/`False`,
floats with a trailing `.0`, and strings single-quoted unless that would mean
escaping an apostrophe (`repr_str`, `:83`). `truthy` (`:70`) is Python's
truthiness — empty containers, zero, `False` and `None` are false — and is what
the tool parsers use to decide whether an argument or id was supplied. Tests:
`dumps_uses_python_separators` (`:195`), `dumps_escapes_non_ascii` (`:205`),
`repr_matches_python` (`:217`), `str_unwraps_only_strings` (`:228`),
`truthiness_follows_python` (`:234`).

`pyfmt` exists because harness values are authored in YAML, rendered into
prompts, and asserted on from Python. A `Value` that renders as `true` on one
side and `True` on the other would make every such assertion a translation
exercise. `json_dumps_indent2` (`crates/kerness/src/pyfmt.rs:25`) is the one
exception to the ASCII rule:
it matches `json.dumps(..., indent=2, ensure_ascii=False)`, which is the session
file shape (`crates/kerness/src/sessionfile.rs:166`).

## Key Types and Entry Points

- `crates/kerness/src/utils.rs:12` — `DEFAULT_TERMINATORS` — `CONSENSUS_REACHED`
  and `END_SESSION`, in priority order; exported to Python as a tuple
  (`bindings/python/src/funcs.rs:679`).
- `crates/kerness/src/utils.rs:34` — `keyword_in_text(text, keyword)` — a
  boundary-aware, case-insensitive scan; `false` for an empty keyword.
- `crates/kerness/src/utils.rs:64` — `parse_session_end(text, keywords)` — the
  first declared terminator present, or `None`.
- `crates/kerness/src/utils.rs:79` — `parse_orchestrator_call(text, agent_names)`
  — `(name, instruction)` for the first roster name mentioned, or `None`.
- `crates/kerness/src/utils.rs:122` — `parse_memory_markers(text)` —
  `(cleaned_text, notes)`; cleaned text is trimmed.
- `crates/kerness/src/utils.rs:179` — `retry(call, retries, backoff_sec,
  interval_sec)` — generic over the error type; sleeps on the calling thread.
- `crates/kerness/src/pyfmt.rs:17` — `json_dumps(value)` — ASCII-only compact
  JSON with Python separators; what tool prompts and native schemas are
  measured and sent as.
- `crates/kerness/src/pyfmt.rs:30` — `repr(value)` / `:62` `str(value)` / `:83`
  `repr_str(text)` — Python's renderings; `str` differs from `repr` only for
  strings.
- `crates/kerness/src/pyfmt.rs:70` — `truthy(value)` — Python truthiness, used
  wherever a parser asks "was this supplied".
- `bindings/python/src/funcs.rs:48` — `parse_memory_markers` through `retry`
  (`:80`) — the five pyfunctions; `retry` drops the GIL while sleeping.

## Interactions

- `keyword_in_text`, `parse_session_end` and `parse_orchestrator_call` are
  called by [loop.md](loop.md) on every orchestrator reply
  (`crates/kerness/src/orchestrator.rs:352`, `:856`, `:879`); the keyword lists
  come from [harness.md](harness.md)'s `terminate_on` and `advance_on`.
- `parse_memory_markers` feeds [memory.md](memory.md) through the session's
  `process_memory_markers` (`crates/kerness/src/session.rs:1612`) and is the
  run's fallback when storing fails (`crates/kerness/src/session/run.rs:1054`).
- `retry` backs [provider.md](provider.md)'s `chat_with_retries`
  (`crates/kerness/src/provider/mod.rs:454`); the provider's retry, backoff and
  interval settings are its arguments.
- `pyfmt` is used wherever a value becomes prompt text, an error message, or a
  tool result: [toolkit.md](toolkit.md)'s parsers (`truthy`, `str`),
  [jsonschema.md](jsonschema.md)'s messages (`repr`, `repr_str`),
  [harness.md](harness.md)'s and [skills.md](skills.md)'s frontmatter errors,
  [sessionfile.md](sessionfile.md)'s identity diff and file body, and the
  bindings' `__repr__` implementations (`bindings/python/src/types.rs:114`).
- [access.md](access.md) compiles caller-supplied command patterns with
  `fancy-regex` (`crates/kerness/src/access.rs:14`) because a caller's pattern
  may use lookaround; the framework's own scanning here stays lookaround-free
  by construction.

## How to Test

```sh
cargo test -p kerness utils                                       # pass = 5 passed, 0 failed
cargo test -p kerness pyfmt                                       # pass = 5 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_utils.py -q # pass = 22 passed
```

- `bindings/python/tests/test_utils.py:13` — `TestParseOrchestratorCall`, `:30`
  `TestParseSessionEnd`, `:44` `TestParseMemoryMarkers` — each parametrises the
  boundary cases its scan exists for; `:60` `TestRetry` covers first-attempt
  success, recovery with and without a fixed interval, and exhaustion.
- The `pyfmt` Rust tests (`crates/kerness/src/pyfmt.rs:195` onward) assert
  against literal strings taken from CPython, which is the only way the
  byte-for-byte claim can be checked.
- Gaps: `pyfmt` has no Python-side test because nothing exports it; its
  behaviour is observed through every prompt and error string the Python suite
  asserts on. `json_dumps_indent2` is asserted only through
  [sessionfile.md](sessionfile.md)'s round trip.

## Review and Refactor Guide

- Changing a scan's boundary rule → the function and its inline test; then
  [loop.md](loop.md)'s orchestrator tests, because routing and termination read
  through it, and `test_utils.py`'s table.
- Changing `retry`'s schedule → [provider.md](provider.md)'s request defaults
  (`DEFAULT_RETRIES`, `DEFAULT_BACKOFF_SEC`) are asserted by
  `crates/kerness/tests/public_api.rs` and `bindings/python/tests/test_provider.py`;
  the Python `retry` signature defaults (`bindings/python/src/funcs.rs:80`)
  spell the same numbers.
- Changing `pyfmt` output → every prompt and error string that models and the
  Python suite read; run the whole gate, not this module's tests alone, and
  compare against CPython's output for the case in hand.
- Adding a helper here → it needs two callers first; a single-caller function
  lives beside its user.
- Forbidden coupling: neither file may import another crate module. A helper
  that needs `error` or `provider` belongs elsewhere.
- Compatibility: `DEFAULT_TERMINATORS` is a documented constant in the root's
  well-known table and exported to Python; changing it changes what every
  built-in gameplan ends on.

Improvement candidates (proposals, not accepted work):

- A `parse_session_end` case in `utils.rs` with a keyword bounded by a
  non-ASCII letter, pinning the byte-wise boundary claim; success check: the
  case passes and documents the UTF-8 argument in code.

## Open Gaps / Roadmap

- `pyfmt` covers the JSON value model only. There is nothing for a Python object
  that is not JSON-representable, and nothing needs it.
- `retry`'s backoff grows linearly, or uses a fixed interval, with no jitter and
  no `Retry-After` handling; see [provider.md](provider.md).
- Every returned error is retried. Classifying a non-retryable failure would be
  a provider-level decision, and none is made here.
