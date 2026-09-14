---
eatmycode_version: "2.0.0"
---

# Providers and model I/O

Owner: [Project architecture](../../ARCHITECTURE.md)

Read when: changing provider transport, credentials, retries, tool wire dialects,
structured output, reasoning effort, context-window declarations or usage budgets.

## Responsibility and Status

Implemented: blocking OpenAI-compatible, Anthropic and OpenRouter/custom backends
normalize responses and share retry/fallback behavior. Owns native tool wire
formats and usage accounting; the run owns scheduling and budget termination.
Tests use mocked/scripted transport. Live endpoint and credential compatibility
is not established by offline verification.

## Code Map

| Path / symbol | Role |
| --- | --- |
| [provider/mod.rs](../../crates/kerness/src/provider/mod.rs), `Provider`, `ProviderBase` | Shared defaults, retry/empty guards, one-way feature fallback and backend exports. |
| [provider/](../../crates/kerness/src/provider) backend files | Vendor requests, authentication headers and response conversion. |
| [http.rs](../../crates/kerness/src/http.rs), `HttpTransport` | Blocking transport seam; Rust default uses ureq/TLS. |
| [toolschema.rs](../../crates/kerness/src/toolschema.rs), `ToolDialect` | OpenAI/Anthropic/text schemas, assistant turns and tool-result serialization. |
| [usage.rs](../../crates/kerness/src/usage.rs), `UsageCollector`; [session_run.rs](../../crates/kerness/tests/session_run.rs), [public_api.rs](../../crates/kerness/tests/public_api.rs) | Observations, optional measurements, host pricing and budget checks; public defaults. |

## Local Conventions

Use [root conventions](../../ARCHITECTURE.md#code-conventions). Supplied provider
behavior is implemented in free functions generic over `P: Provider + ?Sized`
so Python subclass overrides remain effective; do not bypass those dispatch
seams. Keep shared request defaults in Rust constants, exposed through the
binding rather than independent behavioral defaults. `ProviderResponse` keeps
raw response/usage alongside normalized content/calls. Process-wide transport is
a `OnceLock`/`RwLock` slot with a usable Rust default (`http.rs`).

## Contracts and Invariants

- Providers implement `name`, `base`, and `chat`; shared methods supply retries,
  guards and dialect behavior. `retries` counts extra attempts, so zero still
  performs one request (`ProviderBase`, inline provider tests).
- A tool-only response is valid even with empty content. An unreadable/empty
  response must surface the proper framework error rather than look successful.
  Call IDs and ordered tool results survive dialect conversion (`toolschema.rs`).
- HTTP failures retain status, URL and server body; unreadable error bodies carry
  a read diagnostic. Invalid JSON is a provider response error, distinct from
  network/IO failure (`http.rs`). Successful-status API error envelopes retain
  vendor code/message/metadata in the error. OpenAI-compatible `refusal` and
  `content_filter`, and Anthropic `stop_reason: refusal`, fail before accepting
  content or tools. Empty and invalid structured replies retain reported stop
  reasons (`provider/mod.rs`, `openai.rs`). Both session entry points propagate
  unrecovered errors; [runtime](runtime.md) owns the terminal behavior.
- Native-tool rejection and reasoning-effort rejection have separate one-way
  latches in the provider instance. Only matching refusal evidence degrades the
  feature; unrelated failures must not disable it. A mixed session resolves each
  agent's effective dialect independently (`provider/mod.rs`, Python provider tests).
  A concurrent native response retains its declared call/result dialect even if
  a sibling request disables native tools; later requests honor the fallback.
- Context windows are supplied by the host/provider, not a bundled model table.
  The runtime combines these declarations with session limits; output token
  limits do not bound input context. No streaming contract is implemented.
- Raw usage is preserved; normalized missing/invalid/overflowed counts remain
  unknown (`None`), not fabricated zero. Cached/reasoning tokens are subsets,
  not amounts to add twice. Host pricing supplies integer prices; no live price
  registry exists (`usage.rs`).
- Thread-local observation stacks attribute concurrent supplied provider paths
  independently. Nested wrappers transfer a reserved operation to their first
  attempt; retries reserve individually. Provider/tool count checks and capacity
  reservations are atomic across a collector, and admitted completions remain
  recorded after another action exhausts a budget. Overriding dispatch may yield
  an opaque logical operation rather than individual request measurements. Hard
  token/cost caps are rejected; measured thresholds can overshoot by operations
  already in flight (`usage.rs`, `session_run.rs`).
- Provider URLs/credentials are host configuration. HTTP does not cross command
  `AccessManager` and must not be described as confined by `allowed_hosts`.

## Dependencies and Boundaries

[Runtime](runtime.md) supplies resolved agents, request purposes, cancellation and
run accounting scopes. Read it for call-count/budget behavior. Read
[tools/access](tools-access.md) for canonical tool specs or dispatcher changes;
this owner only transforms wire shapes. Read [memory/persistence](memory-persistence.md)
for overflow detection/compaction and memory-maintenance provider calls.
[Python bindings](python-bindings.md) installs a transport resolving
`kerness.provider.http_post_json` per call so monkeypatching works, and owns
signature inspection/Pydantic integration; read it for provider surface changes.

## Change Guide

| Change trigger | Inspect / extend | Required docs / checks |
| --- | --- | --- |
| Backend/request/credentials/retry | Backend and shared dispatch, `http.rs`, provider inline tests | Update this owner and exposed Python API; mocked request/response and default tests. |
| Tool wire dialect or fallback | `toolschema.rs`, degrade latches; `tools_e2e.rs` | Read tools/access and runtime; validate calls/results and mixed dialect turns. |
| Usage/pricing/limit semantics | `usage.rs`, runtime accounting scopes; `session_run.rs` | Read runtime and bindings; unknown usage, nested calls and unsupported caps must remain explicit. |

## Verification

The [root Rust suite](../../ARCHITECTURE.md#verification) runs provider,
toolschema/usage inline tests, `public_api` request defaults, `tools_e2e`
dialects and `session_run` budget/usage cases. The
[Python suite](python-bindings.md#verification), especially `test_provider.py`,
`test_toolschema.py` and `test_session.py`, proves subclass overrides, patched
transport, Pydantic, fallback and measurement translation. See
[baseline evidence](../topics/build-checks.md#evidence-and-gaps).

## Known Gaps

No offline test proves a current vendor endpoint or OAuth credential works.
Opaque provider overrides and failed requests can leave billable usage unknown.
No embedded model limits/prices, hard token/cost guarantees, or forced callback
interruption are implemented; changing these requires an explicit API contract.
