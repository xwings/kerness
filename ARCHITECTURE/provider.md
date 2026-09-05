# Provider

## Goal

Talking to a model. `Provider` is the trait everything else calls: hand it a
model name, messages, a reasoning effort level, and optionally tool schemas, and
get a `ProviderResponse` back. Four backends ship — OpenAI, OpenRouter,
Anthropic, and a `CustomProvider` for an endpoint the caller describes — and the
retry, dialect selection, and the two degrade latches are supplied once for all
of them. M3 adds run accounting and budgets at the supplied dispatch boundary.

`http.rs` underneath is the transport, and it is a seam on purpose: the default
is pure Rust (`ureq` over `rustls`), and the Python binding replaces it with one
that resolves `kerness.provider.http_post_json` at call time so `@patch` works.

## Status

`done` — M3 supplies normalized accounting, host pricing, explicit measurement
limits, and pre-dispatch budget checks without changing existing provider
response fields.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/provider/mod.rs` | the trait, `ProviderResponse`, the supplied methods |
| `crates/kerness/src/provider/openai.rs` | OpenAI chat completions |
| `crates/kerness/src/provider/openrouter.rs` | OpenRouter |
| `crates/kerness/src/provider/claude.rs` | Anthropic messages, API key or OAuth |
| `crates/kerness/src/provider/custom.rs` | a caller-described endpoint |
| `crates/kerness/src/usage.rs` | normalized usage, host pricing, run accounting, cooperative budgets |
| `crates/kerness/src/http.rs` | `HttpTransport`, `UreqTransport`, `post_json` |
| `bindings/python/src/provider.rs` | `PyProviderCore`, `PyProvider`, the transport seam |
| `bindings/python/kerness/provider.py` | the base class and six concrete providers, in Python |

## Key Types and Entry Points

- `crates/kerness/src/provider/mod.rs:194` — `Provider` — required single-request `chat` plus default retry/dialect behavior.
- `crates/kerness/src/provider/mod.rs:115` — `ProviderResponse` — unchanged raw provider fields, tool calls, and structured output.
- `crates/kerness/src/provider/mod.rs:150` — `ProviderBase` — retries, throttling, context window, and degrade latches.
- `crates/kerness/src/provider/mod.rs:432` — `supplied_chat_with_retries` — observable retry attempts and capability fallbacks.
- `crates/kerness/src/provider/mod.rs:479` — `supplied_chat_dispatch` — actual attempt observation and native-tool routing.
- `crates/kerness/src/usage.rs:23` — `NormalizedUsage` — known counts and explicit unknown measurements.
- `crates/kerness/src/usage.rs:218` — `UsageLedger` — checkpointable records and run/actor/provider aggregates.
- `crates/kerness/src/usage.rs:121` — `TokenPricing` — host-supplied integer prices and checked cost calculation.
- `crates/kerness/src/usage.rs:259` — `RunBudget` — exact operation/tool admission and explicit token/cost thresholds.
- `crates/kerness/src/usage.rs:323` — `UsageCollector` — scoped attribution, accounting, and typed budget refusals.

### Why the supplied methods are free functions

A defaulted trait method cannot be called on behalf of a type that overrode it.
The Python binding needs exactly that: a subclass that overrides
`chat_with_retries` must win, but a subclass that does not must get the framework
body. Extracting each body into a free function generic over `P: Provider + ?Sized`
lets the binding call the framework's version explicitly when Python has no
override.

### Run accounting and budgets (M3)

`NormalizedUsage` adds common token counts
without changing `ProviderResponse.usage` or its public struct layout.
OpenAI-compatible prompt/completion counts and Anthropic input/output counts
normalize to input, output, and total tokens. Anthropic cache-read and
cache-creation counts are added to its input count; OpenAI cached tokens and
reasoning tokens remain subsets. Missing, invalid, inconsistent, or overflowing
counts are unknown (`None`). A reported zero stays a known zero.

`UsageCollector` belongs to one run. The
engine installs a synchronous thread-local scope carrying the trusted actor
and call purpose, then wraps engine provider boundaries, including compaction,
closing, and memory maintenance. Tool handlers also run inside the actor's
accounting scope, so calls through supplied provider dispatch are metered and
budgeted during the invocation. Supplied dispatch records each attempted `chat`,
including errors and degrade retries. Nested wrappers do not count the same attempt twice. A custom
override bypassing supplied dispatch contributes one **opaque logical
operation**, with unknown token usage and unknown physical attempt count.
Providers must honor the one-request `chat` contract for exact attempt counts.
Scopes restore on return and unwind; callbacks execute without the collector's
mutex held. Provider work on a custom background thread cannot inherit this
synchronous scope. Direct custom overrides or arbitrary I/O inside a handler
that bypass framework observation remain the host implementation's responsibility.

`UsageLedger` stores operation records, run
totals, tool invocation count, and elapsed milliseconds; it also groups records
by actor or provider. One failed request with missing usage makes its aggregate
unknown; known measurements remain available in the individual records.
Ledger snapshots serialize with version 2 run checkpoints. Restoration checks
that aggregate totals match records and carries forward elapsed active time.
Time spent waiting for the host while the run is live counts; time offline
between saving and restoring does not.

`TokenPricing` is supplied by the host for an
exact provider/model pair. Integer rates are microdollars per million tokens;
costs round up to a microdollar per operation. Cache-specific rates are optional
and require the corresponding cache measurement. Without a separate cache
rate, all input is charged at the supplied input rate. Missing pricing or
usage yields unknown cost. No model-price registry is embedded in the engine.

`RunBudget` checks elapsed time, token, and
cost limits at action boundaries, operation limits before each provider attempt,
and tool limits immediately before a handler starts. Invoked handlers count
even if they fail; denied actions and pending approvals do not. Budget refusals
retain a typed `BudgetExceeded` reason alongside the existing `Error` API.
Operation and tool limits are hard at these synchronous boundaries. Since
providers expose no enforceable per-request token or cost upper bound, hard
token/cost budgets are rejected. Hosts must explicitly select
`BudgetMode::MeasuredThreshold`; one in-flight request can exceed a threshold,
and unknown usage or cost stops the next metered action. Elapsed limits are
cooperative and cannot interrupt blocking provider or user code. An opaque
override's internal retries cannot be constrained by an engine operation cap.

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

### The context window, and why there is no table

`context_window` answers how many tokens a model can hold, and the honest
default is `None`. The framework ships no table of published window sizes: a
table is wrong the week a vendor changes one, wrong silently, and would have to
carry models the framework has never heard of. So the four built-in backends
answer from a figure their config was given — `context_window` on each
`*Config`, threaded to `ProviderBase::with_context_window` — and a caller with a
model registry of their own overrides the trait method and answers from that.

The method takes a *model* even though `supplied_context_window`
does not read it. One `ProviderBase` holds one figure,
which is right for a backend serving one model; a backend serving several with
different windows is exactly the case the argument exists for, and answering it
means overriding.

`None` is not a failure. The session falls back to its own
`max_context_tokens` alone, which is what every session did before any backend
declared a window. What the figure buys is the other direction: a caller whose
ceiling is generous and whose model is small no longer discovers the mismatch as
a provider refusal. See [compaction.md](compaction.md) for how the two are
combined.

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

The Python suite reaches the built-in backends by patching the transport. The
binding installs an `HttpTransport` that looks up
`kerness.provider.http_post_json` on each call rather than capturing it, so a
`@patch` on the module attribute intercepts every provider. Payload construction
and response parsing stay in Rust either way.

### What the Python classes hold

Nothing. `Provider` is the ABC callers subclass and the built-in classes are
thin: each holds a `_core` and forwards. `CustomProvider.model_config` is the
case that shows why — the vendor dict it answers with is the one
`CustomProvider::new` already stores, read back through `PyProviderCore`'s
getter rather than from a second copy kept beside it. The property still returns a fresh dict per call, so a caller
mutating what it got does not reach the backend; what it no longer does is give
the value two owners that can disagree.

## Interactions

- Called by [agent-runtime.md](agent-runtime.md) for every turn.
- Scoped and budgeted by [run.md](run.md), including tool-internal provider
  calls; [memory.md](memory.md) uses the same boundary for maintenance.
- Tool schemas come from [toolschema.md](toolschema.md); the dialect decides the
  wire shape.
- Structured output runs the schema through [jsonschema.md](jsonschema.md)'s
  `ensure_strict` before sending, then `model_validate` on the Python side.
- Failed attempts are retried before error classification. After exhaustion,
  `is_provider()` controls capability fallback and error wrapping;
  `is_context_overflow()` identifies failures [compaction.md](compaction.md)
  can recover from by shrinking the conversation.
- `context_window` is read once per turn by [session.md](session.md)'s
  `context_ceiling`.

## How to Test

```sh
cargo test -p kerness --lib provider
cargo test -p kerness --lib usage
cargo test -p kerness --test tools_e2e
.venv/bin/python -m pytest bindings/python/tests/test_provider.py -q
```

Pass means exit code 0 and no failed tests. Rebuild the Python extension before
running its tests after a Rust change.

The core-upgrade verification passed 39 provider-filtered Rust tests, all six
usage tests, all 18 tool integration tests, and the rebuilt Python
suite. The complete workspace gate is recorded in [testing.md](testing.md).

- `crates/kerness/src/provider/mod.rs:1116` — Success, failed attempts, retry accounting, and an operation cap preventing transport dispatch.
- `crates/kerness/src/provider/mod.rs:1618` — Degrade retries remain separately accounted without duplicate wrapper records.
- `crates/kerness/src/usage.rs:665` — Provider spellings, unknown versus zero, subsets, and invalid/overflowing counts.
- `crates/kerness/src/usage.rs:844` — Unsupported hard limits reject; measured budgets stop subsequent actions.
- `crates/kerness/tests/tools_e2e.rs:180` — Tool-internal provider calls use the trusted actor and cannot bypass run operation limits.

## Open Gaps / Roadmap

- No streaming. A response is one request and one reply; a harness that wants
  token-by-token output cannot get it.
- M3 measurement limits are explicit above: opaque override internals and
  missing provider usage are unknown; hard token/cost reservation requires a
  future enforceable provider upper-bound contract.
- Retry applies to every returned error, with linearly increasing waits unless
  a fixed interval is configured. There is no jitter or `Retry-After` handling.
- `pydantic` is optional and imported lazily;
  structured output raises a clear error when it is absent rather than degrading.
- `context_window` is a figure the caller supplies; nothing checks it against
  what the endpoint will actually accept, so a wrong one is wrong in whichever
  direction it was written.
- The minimum-interval throttle is per provider instance, so two providers
  against one endpoint do not coordinate.
