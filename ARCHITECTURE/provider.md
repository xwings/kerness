# Provider

## Goal

Talking to a model. `Provider` is the trait everything else calls: hand it a
model name, messages, and optionally tool schemas, and get a `ProviderResponse`
back. Four backends ship — OpenAI, OpenRouter, Anthropic, and a `CustomProvider`
for an endpoint the caller describes — and the retry, dialect selection, and
native-tools fallback are supplied once for all of them. Serves **M2**.

`http.rs` underneath is the transport, and it is a seam on purpose: the default
is pure Rust (`ureq` over `rustls`), and the Python binding replaces it with one
that resolves `kerness.provider.http_post_json` at call time so `@patch` works.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/provider/mod.rs` | the trait, `ProviderResponse`, the supplied methods |
| `crates/kerness/src/provider/openai.rs` | OpenAI chat completions |
| `crates/kerness/src/provider/openrouter.rs` | OpenRouter |
| `crates/kerness/src/provider/claude.rs` | Anthropic messages, API key or OAuth |
| `crates/kerness/src/provider/custom.rs` | a caller-described endpoint |
| `crates/kerness/src/http.rs` | `HttpTransport`, `UreqTransport`, `post_json` |
| `bindings/python/src/provider.rs` | `PyProviderCore`, `PyProvider`, the transport seam |
| `bindings/python/kerness/provider.py` | the base class and six concrete providers, in Python |

## Key Types and Entry Points

- `crates/kerness/src/provider/mod.rs:102` — `Provider` — `chat` is required; four
  methods are defaulted.
- `crates/kerness/src/provider/mod.rs:38` — `ProviderResponse` — text, tool calls,
  and an optional `structured` value for structured output.
- `crates/kerness/src/provider/mod.rs:73` — `ProviderBase` — retries, backoff, and
  minimum interval, shared by every backend.
- `crates/kerness/src/provider/mod.rs:186`–`:263` — `supplied_effective_dialect`,
  `supplied_note_native_tools_rejected`, `supplied_chat_with_retries`,
  `supplied_chat_dispatch` — the four defaulted method bodies, extracted as free
  functions generic over `P: Provider + ?Sized`.
- `crates/kerness/src/provider/mod.rs:373` — `convert_messages_for_claude(messages)` —
  the Anthropic wire shape, which lifts the system message out of the list.
- `crates/kerness/src/http.rs:24` — `HttpTransport` — the seam; `:73`
  `set_transport` installs a replacement; `:78` `post_json` is what providers call.
- `bindings/python/kerness/provider.py:64` — `Provider` — the class user code subclasses.
- `bindings/python/kerness/provider.py:86` — `effective_dialect()` and `:102`
  `_chat_accepts_tools()` — deliberately Python: it is
  `inspect.signature(type(self).chat)` on the concrete subclass, and the core
  takes the answer as a capability flag.
- `bindings/python/src/provider.rs:596` — `install_transport()` — installs the
  transport that resolves `kerness.provider.http_post_json` at call time.
- `bindings/python/src/provider.rs:607` — `http_post_json(...)` — the unpatched
  function, itself a `#[pyfunction]` over the same Rust code.

### Why the supplied methods are free functions

A defaulted trait method cannot be called on behalf of a type that overrode it.
The Python binding needs exactly that: a subclass that overrides
`chat_with_retries` must win, but a subclass that does not must get the framework
body. Extracting each body into a free function generic over `P: Provider + ?Sized`
lets the binding call the framework's version explicitly when Python has no
override.

### Why `@patch` works

24 tests patch the transport. The binding installs an `HttpTransport` that looks
up `kerness.provider.http_post_json` on each call rather than capturing it, so a
`@patch` on the module attribute intercepts every provider. Payload construction
and response parsing stay in Rust either way.

## Interactions

- Called by [agent-runtime.md](agent-runtime.md) for every turn.
- Tool schemas come from [toolschema.md](toolschema.md); the dialect decides the
  wire shape.
- Structured output runs the schema through [jsonschema.md](jsonschema.md)'s
  `ensure_strict` before sending, then `model_validate` on the Python side.
- Failures become `ProviderError` and its subclasses in [errors.md](errors.md);
  `is_provider()` is what makes them retryable.

## How to Test

```sh
cargo test -p kerness provider                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_provider.py -q # pass = 0 failed
```

- The Rust tests use a `Recorder` transport (`provider/mod.rs:451`) to assert the
  exact payload each backend builds, without a network call.
- `bindings/python/tests/test_provider.py` covers the `@patch` seam, the native-tools fallback
  when a provider rejects tool schemas, retry and backoff, and structured output
  through `pydantic`.

## Open Gaps / Roadmap

- No streaming. A response is one request and one reply; a harness that wants
  token-by-token output cannot get it.
- Retry is a fixed backoff over `is_provider()` errors; there is no jitter and no
  respect for a `Retry-After` header.
- `pydantic` is optional and imported lazily (`bindings/python/kerness/provider.py:48`);
  structured output raises a clear error when it is absent rather than degrading.
- The minimum-interval throttle is per provider instance, so two providers
  against one endpoint do not coordinate.
