---
eatmycode_version: "1.2.0"
---

# Provider

## Goal

Talking to a model. `Provider` is the trait everything else calls: hand it a
model name, messages, a reasoning effort level, and optionally tool schemas, and
get a `ProviderResponse` back. Four backends ship — OpenAI, OpenRouter,
Anthropic, and a `CustomProvider` for an OpenAI-compatible endpoint the caller
describes — and the retry, dialect selection, and the two degrade latches are
supplied once for all of them. The run accounting and budgets in `usage.rs`
sit at the supplied dispatch boundary and belong here too.

`http.rs` underneath is the transport, and it is a seam on purpose: the default
is pure Rust (`ureq` over `rustls`), and the Python binding replaces it with one
that resolves `kerness.provider.http_post_json` at call time so `@patch` works.

This module does not own which agent calls which provider, or how a turn feeds
tool results back — [session.md](session.md) and
[agent-runtime.md](agent-runtime.md) do — nor the wire shape of a tool
definition, which is [toolschema.md](toolschema.md)'s.

## Status

`done` — `cargo test -p kerness --lib provider` passes 39 tests,
`cargo test -p kerness --lib usage` passes 6,
`cargo test -p kerness --test tools_e2e` passes 18, and
`.venv/bin/python -m pytest bindings/python/tests/test_provider.py -q` passes
59. Every test drives a recorded transport; nothing reaches the network.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/provider/mod.rs` | `Provider`, `ProviderBase`, `ProviderResponse`, `ReasoningEffort`, the request defaults, the supplied bodies, and the shared payload/decode helpers |
| `crates/kerness/src/provider/openai.rs` | OpenAI chat completions, with structured output |
| `crates/kerness/src/provider/openrouter.rs` | OpenRouter, with attribution headers |
| `crates/kerness/src/provider/claude.rs` | Anthropic messages, API key or OAuth |
| `crates/kerness/src/provider/custom.rs` | a caller-described OpenAI-compatible endpoint |
| `crates/kerness/src/usage.rs` | normalized usage, host pricing, the run ledger, budgets, and the thread-local accounting scope |
| `crates/kerness/src/http.rs` | `HttpTransport`, `UreqTransport`, the transport slot, `post_json` |
| `bindings/python/src/provider.rs` | `PyProviderCore`, `PyProvider`, `bind_provider`, `PyTransport`, `http_post_json` |
| `bindings/python/kerness/provider.py` | the `Provider` ABC and six thin concrete classes |

## Language and Conventions

Rust crate modules, one PyO3 binding module, and one Python module that is
deliberately more than a shim. The root's [Coding Style and Code
Design](../ARCHITECTURE.md#coding-style-and-code-design) rules apply. Local
facts:

- `Provider` is a Python ABC (`bindings/python/kerness/provider.py:99`)
  because callers subclass it and `isinstance` has to agree. Its methods
  forward `self` back into `PyProviderCore`, so a subclass override wins by
  ordinary method resolution. The one introspective step in the framework is
  `_signature_accepts` (`:66`), which reads a subclass's `chat` signature with
  `inspect` to decide whether it can be offered `tools` or `reasoning_effort`.
- The four `#[allow(clippy::too_many_arguments)]` sites in
  `bindings/python/src/provider.rs` (`:174`, `:221`, `:276`, `:323`) are the
  backend constructors; each Python signature is spelled in the
  `#[pyo3(signature)]` directly above.
- The transport is a process-global `OnceLock<RwLock<Arc<dyn HttpTransport>>>`
  slot (`crates/kerness/src/http.rs:69`); lock poisoning is
  `expect("transport lock poisoned")`. Provider unit tests install a recording
  transport under a static mutex (`install`,
  `crates/kerness/src/provider/mod.rs:758`) because the slot is shared by
  concurrent tests.
- Every checkpointable type in `usage.rs` is `#[serde(deny_unknown_fields)]`
  (`:22`, `:120`, `:165`, `:179`, `:217`, `:258`); `BudgetMode` and
  `BudgetExceeded` are `rename_all = "snake_case"` wire enums.
- The two degrade latches are `AtomicBool`s (`crates/kerness/src/provider/mod.rs:155`),
  not locked, because each moves in one direction only.
- Python tests patch `kerness.provider.http_post_json`
  (`bindings/python/tests/test_provider.py:95`) and use `Test<Behaviour>`
  classes; Rust tests use `Recorder` (`crates/kerness/src/provider/mod.rs:705`)
  with a queue of canned replies whose last entry repeats.
- `pydantic` is optional and imported lazily by `_require_pydantic`
  (`bindings/python/kerness/provider.py:83`); structured output raises an
  `ImportError` naming the extra rather than degrading.

## Design and Invariants

### Dependency direction

`provider/mod.rs` imports `error`, `http`, `logging`, `pyfmt`, `tooling`,
`toolschema`, `usage`, and `utils`; `openai.rs` also imports `jsonschema`.
`usage.rs` imports only `error` and `provider::ProviderResponse`. Nothing here
imports `agent`, `session`, or `memory`; the callers are
[agent-runtime.md](agent-runtime.md), [session.md](session.md),
[compaction.md](compaction.md), and [memory.md](memory.md), each through
`chat_with_retries`.

### Why the supplied methods are free functions

A defaulted trait method cannot be called on behalf of a type that overrode it.
The Python binding needs exactly that: a subclass that overrides
`chat_with_retries` must win, but one that does not must get the framework
body. Each supplied body is a free function generic over `P: Provider + ?Sized`
(`supplied_effective_dialect`, `crates/kerness/src/provider/mod.rs:318`,
through `supplied_chat_dispatch`, `:479`), and the trait's default methods
call them. `PyProviderCore` calls the free functions explicitly
(`bindings/python/src/provider.rs:444`, `:469`) while `PyProvider` routes every
trait method back to the Python object (`:517`), so the override and the
default are never the same call.

### One request, one response

`Provider::chat` (`crates/kerness/src/provider/mod.rs:208`) is one request.
`chat_with_retries` (`:288`) wraps it in `utils::retry`
(`crates/kerness/src/utils.rs:179`) with `retries` *extra* attempts, so `0`
still calls once, and treats a reply with neither text nor tool calls as
`Error::ProviderEmpty` (`crates/kerness/src/provider/mod.rs:447`). After exhaustion, a provider error is handed
to the two latches in order — tools first, then effort — and the request is
retried once without the refused part; anything else becomes
`Error::Provider("All retries exhausted …")` (`crates/kerness/src/provider/mod.rs:471`). Enforced by
`the_budget_is_spent_only_on_failure_and_then_reported` (`crates/kerness/src/provider/mod.rs:1116`),
`a_rejected_endpoint_retries_once_without_tools` (`crates/kerness/src/provider/mod.rs:1679`), and
`a_second_refusal_is_reported_rather_than_retried` (`crates/kerness/src/provider/mod.rs:1652`).

### The two degrade latches

`ProviderBase` holds two `AtomicBool`s, and neither ever flips back: a latch
that reset would put two payload shapes in one conversation. Only a 400, 404,
or 422 is interpretable (`interpretable_refusal`, `crates/kerness/src/provider/mod.rs:365`); a 500 or a timeout
says nothing about parameter support and must not degrade a session for life.
`note_native_tools_rejected` (`crates/kerness/src/provider/mod.rs:376`) drops to `ToolDialect::Text` when the
body names `tool`; `note_reasoning_effort_rejected` (`crates/kerness/src/provider/mod.rs:399`) drops the effort
key when the body names any of the four spellings. The effort latch reports
itself once — it sets with `swap` (`crates/kerness/src/provider/mod.rs:419`) — which is load-bearing: the tools
retry guards re-entry by passing `tools: None`, but the effort retry re-sends
identical arguments, so the latch is the only thing that ends the recursion.
`High` is a default that is *sent*, so a session against a model with no
effort parameter spends one rejected request before the latch fires, once per
provider, logged. Enforced by
`a_400_naming_tools_latches_down_to_text_for_good` (`crates/kerness/src/provider/mod.rs:1321`),
`a_failure_that_is_not_about_tools_does_not_latch` (`:1335`),
`a_400_naming_the_effort_parameter_latches_it_off_for_good` (`:1578`), and
`a_model_with_no_reasoning_mode_retries_once_without_the_level` (`:1618`).

### Three tiers decide the dialect

`supplied_effective_dialect` (`crates/kerness/src/provider/mod.rs:318`) checks the latch, then the declared
`tool_dialect`, then `accepts_tools`. The last is always true for a Rust
implementation, whose signature says so; the binding answers it by inspecting
the subclass's `chat` (`bindings/python/src/provider.rs:562`), which keeps a
hand-written test double that never declared `tools` working untouched. An
empty `tools: []` is a 400 at OpenAI, so `attach_tool_schemas` (`crates/kerness/src/provider/mod.rs:662`) leaves
the key off entirely when there is nothing to send. Enforced by
`a_declared_dialect_wins_when_chat_can_carry_tools` (`crates/kerness/src/provider/mod.rs:1279`),
`a_chat_that_cannot_carry_tools_falls_back_to_text` (`:1285`), and
`no_tools_key_when_there_is_nothing_to_send` (`:1375`).

### The reasoning effort level

A level travels per turn, read off the agent making the call
([agent.md](agent.md)), because two agents sharing one provider may think at
different depths. Each backend renders it in its own wire shape; there is no
shared spelling:

| Backend | Key |
| --- | --- |
| `openai.rs:148` | `"reasoning_effort": "high"` |
| `custom.rs:150` | `"reasoning_effort": "high"`, inserted before the `extra_body` merge so a vendor spelling it otherwise can overwrite the key |
| `openrouter.rs:128` | `"reasoning": {"effort": "high"}` |
| `claude.rs:150` | `"output_config": {"effort": "high"}` |

Anthropic accepts a narrower set of names than the enum offers, and nothing
remaps a level the model has no word for — that is a rejection, and the
rejection is what the second latch is for. Enforced by
`each_backend_spells_the_effort_level_its_own_way` (`crates/kerness/src/provider/mod.rs:1520`) and
`an_extra_body_outranks_the_effort_key_it_shares_a_name_with` (`:1557`).

### The context window, and why there is no table

`context_window` answers how many tokens a model can hold, and the honest
default is `None`. The framework ships no table of published window sizes: a
table is wrong the week a vendor changes one, wrong silently, and would have to
carry models the framework has never heard of. The four backends answer from
the figure their config was given, threaded to
`ProviderBase::with_context_window` (`crates/kerness/src/provider/mod.rs:178`); a caller with a model registry of
their own overrides the trait method. The method takes a *model* even though
`supplied_context_window` (`crates/kerness/src/provider/mod.rs:338`) does not read it: one `ProviderBase` holds
one figure, and a backend serving several models is exactly the case the
argument exists for. `None` is not a failure — the session falls back to its
own `max_context_tokens` alone ([compaction.md](compaction.md)). Enforced by
`TestContextWindow` (`bindings/python/tests/test_provider.py:778`).

### Run accounting and budgets

`NormalizedUsage::from_reported` (`crates/kerness/src/usage.rs:35`) reads
OpenAI-compatible prompt/completion counts and Anthropic input/output counts
into input, output, and total. Anthropic's cache-read and cache-creation counts
are added to its input; OpenAI's cached and reasoning tokens stay subsets.
Missing, invalid, inconsistent, or overflowing counts are `None`; a reported
zero stays a known zero. `ProviderResponse.usage` is left untouched.

`UsageCollector` (`:323`) belongs to one run. The engine installs a synchronous
thread-local scope carrying the trusted actor and purpose (`with_scope`,
`:394`) and wraps every engine provider boundary, including compaction,
closing, and memory maintenance, through `provider_call` (`:406`). Tool
handlers run inside the actor's scope, so a provider call made from inside a
tool through supplied dispatch is metered and budgeted under that actor.
Supplied dispatch records each attempted `chat`, including errors and degrade
retries, through `observe_attempt` (`:619`); nested wrappers do not count the
same attempt twice (`observe`, `:575`). A custom override bypassing supplied
dispatch contributes one **opaque** operation with unknown usage and unknown
attempt count. Scopes restore on return and on unwind (`ScopeGuard`, `:561`);
callbacks execute without the collector's mutex held; provider work on a
caller's own thread cannot inherit the scope.

`UsageLedger` (`:218`) holds records, totals, tool invocation count, and
elapsed milliseconds, and groups by actor or provider. One failed request with
missing usage makes its aggregate unknown while the individual records keep
their known counts. `restore` (`:330`) rejects a ledger whose totals disagree
with its records and carries elapsed active time forward; time offline between
save and restore does not count.

`TokenPricing` (`:121`) is host-supplied per exact provider/model pair, in
microdollars per million tokens, rounded up per operation; a cache rate is
optional and needs the matching measurement. No price registry is embedded.

`RunBudget` (`:259`) checks elapsed, token, and cost limits at every action
boundary, operation limits before each provider attempt, and tool limits
immediately before a handler starts (`begin_tool`, `:387`; invoked handlers
count even when they fail, denied or pending actions do not). Hard token and
cost limits are rejected by `validate` (`:270`) because a provider exposes no
enforceable per-request upper bound; a host selects
`BudgetMode::MeasuredThreshold` explicitly, one in-flight request can exceed
it, and unknown usage or cost stops the next metered action. Elapsed limits are
cooperative. A refusal sets a typed `BudgetExceeded` (`:287`) that the run
reports beside the `Error`.

Cleanup runs under `without_provider_calls` (`:522`): a framework provider call
started during cleanup is refused, not charged, and reported even if a callback
catches it. Enforced by `budgets_gate_next_actions_and_reject_unprovable_hard_limits`
(`:844`), `scopes_restore_on_return_and_unwind_without_cross_run_attribution`
(`:785`), and
`unknown_measurements_stop_metered_runs_and_checkpoints_keep_budget_spent`
(`:917`).

### Structured output

`OpenAiProvider::new` (`crates/kerness/src/provider/openai.rs:84`) builds the
`response_format` once at construction, running the schema through
`jsonschema::ensure_strict` when `strict_json_schema` is set. A tool-calling
turn has no JSON body to validate, so `structured` is filled only when the
reply carries no tool calls (`:160`). A reply that is not JSON is
`Error::Provider` naming the response *shape* — key list and choice count —
rather than the body (`decode_structured`, `:172`). On the Python side
`OpenAIProvider.chat` (`bindings/python/kerness/provider.py:331`) validates the
same reply through the caller's pydantic `TypeAdapter`, which is the one place
a Python object outlives the boundary. Enforced by
`structured_output_builds_a_response_format`
(`crates/kerness/src/provider/mod.rs:901`),
`strict_mode_is_what_rewrites_the_schema` (`:951`),
`structured_output_is_skipped_on_a_tool_calling_turn` (`:1462`), and
`TestOpenAIChat` (`bindings/python/tests/test_provider.py:127`).

### Why `@patch` works

The binding installs a `PyTransport` (`bindings/python/src/provider.rs:719`)
that looks up `kerness.provider.http_post_json` on each call rather than
capturing it, so a `@patch` on the module attribute intercepts every provider.
The unpatched `http_post_json` (`:758`) calls `UreqTransport` directly with the
GIL released — going back through the installed transport would be the
function calling itself. Payload construction and response parsing stay in Rust
either way.

### What the Python classes hold

Nothing. Each built-in class holds a `_core` and forwards.
`CustomProvider.model_config` (`bindings/python/kerness/provider.py:529`) reads
the vendor dict back through `PyProviderCore`'s getter rather than a second
copy, so the value has one owner; the property still returns a fresh dict per
call. A core built for a backend shares that backend's `ProviderBase`
(`Backing::Backend`, `bindings/python/src/provider.rs:51`) rather than keeping
a second latch, because a 400 the endpoint returns has to be visible to the
code that builds the next payload. `bind_provider` (`:682`) accepts a subclass
that never called `Provider.__init__` and gives it the default retry budget.

## Key Types and Entry Points

- `crates/kerness/src/provider/mod.rs:194` — `Provider` — `name`, `base`, and
  the required single-request `chat(model, messages, tools, effort)`; every
  other method is supplied.
- `crates/kerness/src/provider/mod.rs:115` — `ProviderResponse` — `content`,
  answering `model`, raw `usage`, `raw` body, optional `structured` JSON, the
  `tool_calls` in order, and `stop_reason`; `text(content)` at `:135` builds a
  bare reply.
- `crates/kerness/src/provider/mod.rs:150` — `ProviderBase` — retries,
  backoff or fixed interval, the optional context window, and both latches;
  `new(retries, backoff_sec, interval_sec)` at `:161`, where `retries` is extra
  attempts.
- `crates/kerness/src/provider/mod.rs:58` — `ReasoningEffort` — the closed set
  `minimal` through `max`, `High` by default; `parse` at `:83` is
  `Error::Value` on an unknown word.
- `crates/kerness/src/provider/mod.rs:432` — `supplied_chat_with_retries` —
  retry, the empty-reply guard, and the two degrade retries; returns the
  original provider error or `Error::Provider("All retries exhausted …")`.
- `crates/kerness/src/provider/mod.rs:479` — `supplied_chat_dispatch` — one
  observed attempt; passes `tools` only when the effective dialect can carry
  them.
- `crates/kerness/src/provider/mod.rs:40` — `DEFAULT_REQUEST_TIMEOUT_SEC`
  through `DEFAULT_TOP_P` at `:48`, plus `DEFAULT_CLAUDE_MAX_TOKENS`
  (`crates/kerness/src/provider/claude.rs:26`) and the three base URLs — the
  request defaults declared once and named on both sides.
- `crates/kerness/src/usage.rs:323` — `UsageCollector` — `new`/`restore` with a
  validated budget and pricing, `with_scope`, `provider_call`, `begin_tool`,
  `check_next`, `snapshot`, `blocked_reason`.
- `crates/kerness/src/usage.rs:259` — `RunBudget` — the five optional limits
  and `mode`; `validate` refuses hard token or cost limits.
- `crates/kerness/src/http.rs:80` — `post_json(url, payload, headers,
  timeout_sec)` — the one call every built-in backend makes, through whatever
  `set_transport` (`:75`) installed; `Error::ProviderHttp` on a non-2xx status,
  `Error::ProviderNetwork` otherwise.

## Interactions

- [agent-runtime.md](agent-runtime.md) — calls `chat_with_retries` inside
  `observe_provider_call` for every turn
  (`crates/kerness/src/agent_runtime.rs:451`); the contract is one logical
  request per `advance`.
- [session.md](session.md) — resolves the provider per agent (`provider_for`,
  `crates/kerness/src/session.rs:426`) and the dialect (`dialect_for`, `:431`);
  reads `context_window` once per turn in `context_ceiling` (`:1736`).
- [run.md](run.md) — owns the `UsageCollector` and scopes every provider,
  compaction, tool, and maintenance step (`crates/kerness/src/session/run.rs:743`,
  `:813`, `:932`, `:1170`); a tool's internal provider calls inherit the actor
  and budget, tested by
  `contextual_tools_keep_actor_scope_and_expire_after_the_invocation`
  (`crates/kerness/tests/tools_e2e.rs:180`).
- [compaction.md](compaction.md) and [memory.md](memory.md) — the summarizer
  and consolidation calls go through the same boundary
  (`crates/kerness/src/session.rs:1818`, `crates/kerness/src/memory.rs:602`).
- [toolschema.md](toolschema.md) — `tool_schemas` builds the native `tools`
  array and `parse_openai_tool_calls`/`parse_anthropic_tool_calls` read the
  calls back; the dialect decides the wire shape.
- [jsonschema.md](jsonschema.md) — `ensure_strict` rewrites a structured-output
  schema before it is sent.
- [errors.md](errors.md) — `Error::is_provider` gates the latches after
  retry exhaustion; `is_context_overflow` is what
  [compaction.md](compaction.md)'s reactive pass reads.
- [bindings.md](bindings.md) — installs `PyTransport` at bootstrap; the
  `ProviderResponse` pyclass is `bindings/python/src/types.rs:354`.
- [testing.md](testing.md) — `ScriptedProvider`
  (`crates/kerness/tests/common/mod.rs:97`) and `conftest.MockProvider` are
  external `Provider` implementations built on the public trait, which
  `a_provider_written_outside_the_crate_is_a_provider`
  (`crates/kerness/tests/public_api.rs:258`) asserts.

## How to Test

```sh
cargo test -p kerness --lib provider                                  # pass = 39 passed, 0 failed
cargo test -p kerness --lib usage                                     # pass = 6 passed, 0 failed
cargo test -p kerness --test tools_e2e                                # pass = 18 passed, 0 failed
cargo test -p kerness --test public_api the_shared_request_defaults   # pass = 1 passed
.venv/bin/python -m pytest bindings/python/tests/test_provider.py -q  # pass = 59 passed
```

Rebuild the Python extension before running its tests after a Rust change.

- `crates/kerness/src/provider/mod.rs:1116` —
  `the_budget_is_spent_only_on_failure_and_then_reported` — success, failed
  attempts, retry accounting, and an operation cap preventing transport
  dispatch.
- `crates/kerness/src/provider/mod.rs:1618` —
  `a_model_with_no_reasoning_mode_retries_once_without_the_level` — and
  `:1679` `a_rejected_endpoint_retries_once_without_tools`: each degrade retry
  is accounted separately with no duplicate wrapper record.
- `crates/kerness/src/provider/mod.rs:1259` —
  `every_provider_refuses_a_body_it_cannot_read` — a malformed envelope is an
  error, never a `ProviderResponse` carrying junk.
- `crates/kerness/src/provider/mod.rs:1036` —
  `claude_takes_the_system_prompt_as_its_own_field` — and `:1093`
  `every_system_message_is_lifted_out_and_none_is_lost`.
- `crates/kerness/src/usage.rs:665` —
  `normalization_preserves_unknown_zero_and_vendor_subsets` — provider
  spellings, unknown versus zero, subsets, invalid and overflowing counts.
- `crates/kerness/src/usage.rs:844` —
  `budgets_gate_next_actions_and_reject_unprovable_hard_limits`.
- `crates/kerness/tests/tools_e2e.rs:180` — tool-internal provider calls use
  the trusted actor and cannot bypass run operation limits.
- `crates/kerness/tests/public_api.rs:70` — `the_shared_request_defaults_hold`
  — and `bindings/python/tests/test_provider.py:833` `TestSharedDefaults`: the
  constants and every constructor default, asserted on both sides.
- `bindings/python/tests/test_provider.py:479` — `TestDialectDetection` — the
  `inspect`-based capability probe; `:657` `TestReasoningEffort` — the level
  crossing as a string and a `chat` that never declared it never being offered
  one.
- Gap: no test drives `PyProvider::context_window` returning a non-integer from
  Python, which `bindings/python/src/provider.rs:585` folds to `None`.

## Review and Refactor Guide

- Adding a backend → a `*Config` with `Default` built from the constants at
  `crates/kerness/src/provider/mod.rs:40`, a `Provider` impl with `name`,
  `base`, `tool_dialect`, and one `chat`; `pub use` in `crates/kerness/src/provider/mod.rs:26`; a
  `PyProviderCore` static constructor (`bindings/python/src/provider.rs:159`
  onward) and a Python class in `provider.py` whose defaults are the same
  constants; a row in the effort table above; and cases in
  `each_backend_spells_the_effort_level_its_own_way` and
  `TestSharedDefaults`. Reuse `chat_completions_payload` (`crates/kerness/src/provider/mod.rs:498`),
  `bearer_headers` (`:518`), `post_chat_completions` (`:595`), and
  `attach_tool_schemas` (`:662`) for an OpenAI-shaped endpoint.
- Changing a request default → the constant only; both
  `the_shared_request_defaults_hold` and
  `test_the_constants_carry_the_frameworks_values` name the values, and the
  well-known constants table in [runtime.md](runtime.md) lists them.
- Changing retry or latch behaviour → `supplied_chat_with_retries` (`crates/kerness/src/provider/mod.rs:432`)
  and the latch functions (`:376`, `:399`); the Python `Provider` methods that
  forward to them; and the `TestChatWithRetries`, `TestDialectDetection`, and
  `TestReasoningEffort` classes.
- Changing usage or budget shapes → every `deny_unknown_fields` type in
  `usage.rs` is part of the version-2 checkpoint ([sessionfile.md](sessionfile.md));
  a new field needs `restore` (`crates/kerness/src/usage.rs:330`) to accept old ledgers, and
  `unknown_measurements_stop_metered_runs_and_checkpoints_keep_budget_spent`
  (`:917`) is where round-trips are asserted.
- Forbidden coupling: nothing here may import `session`, `agent`, or `memory`;
  a backend must not read the transport slot directly but through
  `http::post_json`; a backend must not cache a dialect outside `ProviderBase`.
- Compatibility: the Python constructor keyword names and defaults are public
  (`test_every_constructor_defaults_to_the_constants`), `ProviderResponse`'s
  fields are serialized into checkpoints, and `chat`'s keyword names `tools`
  and `reasoning_effort` are what the signature probe looks for.

Improvement candidates (proposals, not accepted work):

- Honour `Retry-After` and add jitter in `utils::retry`. Benefit: a
  rate-limited endpoint is not hammered on a fixed schedule. Check: a scripted
  429 with the header is retried no sooner than it asks, and
  `the_budget_is_spent_only_on_failure_and_then_reported` still counts attempts
  exactly.
- Recognise the effort refusal by the vendor's own error code where one exists,
  rather than by substring. Benefit: a body that mentions `reasoning` for an
  unrelated reason does not latch. Check:
  `a_failure_that_is_not_about_the_effort_parameter_does_not_latch` (`crates/kerness/src/provider/mod.rs:1595`)
  gains such a body.

## Open Gaps / Roadmap

- No streaming. A response is one request and one reply; a harness that wants
  token-by-token output cannot get it (M4).
- Measurement limits are explicit above: opaque override internals and missing
  provider usage are unknown, and hard token or cost reservation needs a
  provider contract with an enforceable per-request upper bound (M4).
- Retry applies to every returned error, with linearly increasing waits unless
  a fixed interval is configured. There is no jitter or `Retry-After` handling.
- `context_window` is a figure the caller supplies; nothing checks it against
  what the endpoint will accept, so a wrong one is wrong in whichever direction
  it was written.
- `interval_sec` is only the fixed wait between retry attempts
  (`crates/kerness/src/provider/mod.rs:454`, `crates/kerness/src/utils.rs:196`);
  nothing paces successful requests, and two providers against one endpoint
  do not coordinate.
- Three dialects cover the four backends. A `CustomProvider` against an
  endpoint with a fourth tool shape has to use the text protocol.
