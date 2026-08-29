# Provider

## Goal

Talking to a model. `Provider` is the trait everything else calls: hand it a
model name, messages, a reasoning effort level, and optionally tool schemas, and
get a `ProviderResponse` back. Four backends ship — OpenAI, OpenRouter,
Anthropic, and a `CustomProvider` for an endpoint the caller describes — and the
retry, dialect selection, and the two degrade latches are supplied once for all
of them. Serves **M2**.

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

- `crates/kerness/src/provider/mod.rs:160` — `Provider` — `chat` is required; six
  methods are defaulted.
- `crates/kerness/src/provider/mod.rs:94` — `ProviderResponse` — text, tool calls,
  and an optional `structured` value for structured output.
- `crates/kerness/src/provider/mod.rs:129` — `ProviderBase` — retries, backoff,
  minimum interval, and the two latches, shared by every backend.
- `crates/kerness/src/provider/mod.rs:37` — `ReasoningEffort` — `Minimal`, `Low`,
  `Medium`, `High`, `XHigh`, `Max`, defaulting to `High`; a closed enum with
  `as_str`, `parse`, and `Display`, like `Role`.
- `crates/kerness/src/provider/mod.rs:269`–`:445` — `supplied_effective_dialect`,
  `supplied_effective_effort`, `supplied_note_native_tools_rejected`,
  `supplied_note_reasoning_effort_rejected`, `supplied_chat_with_retries`,
  `supplied_chat_dispatch` — the six defaulted method bodies, extracted as free
  functions generic over `P: Provider + ?Sized`.
- `crates/kerness/src/provider/mod.rs:519` — `convert_messages_for_claude(messages)` —
  the Anthropic wire shape, which lifts the system message out of the list.
- `crates/kerness/src/http.rs:24` — `HttpTransport` — the seam; `:73`
  `set_transport` installs a replacement; `:78` `post_json` is what providers call.
- `bindings/python/kerness/provider.py:81` — `Provider` — the class user code subclasses.
- `bindings/python/kerness/provider.py:103` — `effective_dialect()`, `:119`
  `_chat_accepts_tools()`, and `:123` `_chat_accepts_reasoning_effort()` —
  deliberately Python: each is `inspect.signature(type(self).chat)` on the
  concrete subclass, and the core takes the answer as a capability flag.
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

### The reasoning effort level

A level travels per turn, read off the agent making the call
([agent.md](agent.md)), because two agents sharing one provider may think at
different depths. Each backend renders it in its own wire shape; there is no
shared spelling:

| Backend | Key |
| --- | --- |
| `openai.rs` | `"reasoning_effort": "high"` |
| `custom.rs` | `"reasoning_effort": "high"`, inserted before the `extra_body` merge so a vendor spelling it otherwise can overwrite the key |
| `openrouter.rs` | `"reasoning": {"effort": "high"}` |
| `claude.rs` | `"output_config": {"effort": "high"}` |

Anthropic accepts a narrower set of names than the enum offers, and nothing
remaps a level the model has no word for — that is a rejection, and the rejection
is what the second latch is for.

### The two degrade latches

`ProviderBase` holds two `AtomicBool`s, and neither ever flips back: a latch that
reset would put two payload shapes in one conversation.
`note_native_tools_rejected` drops to the `TEXT` dialect, and
`note_reasoning_effort_rejected` drops the effort key. Both are consulted by the
error arm of `supplied_chat_with_retries`, tools first, so a body naming both
sheds the schemas on the first retry and the level on the second.

The effort latch reports itself once — it sets with `swap`, so a second call
returns `false`. This is load-bearing rather than tidy: the tools retry guards
re-entry by passing `tools: None`, but the effort retry re-sends identical
arguments, so the latch is the only thing that ends the recursion. `high` is a
default that is *sent* rather than a stand-in for "unset", so a session against a
model with no effort parameter spends one rejected request before the latch
fires, once per provider, logged.

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

- The Rust tests use a `Recorder` transport (`provider/mod.rs:603`) to assert the
  exact payload each backend builds, without a network call.
- `bindings/python/tests/test_provider.py` covers the `@patch` seam, the native-tools fallback
  when a provider rejects tool schemas, the effort level reaching each backend's
  own key, retry and backoff, and structured output through `pydantic`.

## Open Gaps / Roadmap

- No streaming. A response is one request and one reply; a harness that wants
  token-by-token output cannot get it.
- Retry is a fixed backoff over `is_provider()` errors; there is no jitter and no
  respect for a `Retry-After` header.
- `pydantic` is optional and imported lazily (`bindings/python/kerness/provider.py:48`);
  structured output raises a clear error when it is absent rather than degrading.
- The minimum-interval throttle is per provider instance, so two providers
  against one endpoint do not coordinate.
