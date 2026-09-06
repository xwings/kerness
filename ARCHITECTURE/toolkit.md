---
eatmycode_version: "1.1.0"
---

# Toolkit

## Goal

Tools, end to end: what a tool is (`ToolSpec`), how a call is recognised in a
model's reply (`parse_tool_calls`), how tools are described to a model that has
no native tool support (`format_tools_prompt`), and how a call is executed and
its result shaped (`ToolDispatcher`).

`tooling.rs` owns the data and the text parsing; `toolkit.rs` owns dispatch.
Neither owns the native wire shapes ([toolschema.md](toolschema.md)), the
schema rules ([jsonschema.md](jsonschema.md)), the permission checks a built-in
handler makes ([access.md](access.md)), or the invocation identity and scoped
capabilities a contextual handler receives ([run.md](run.md)).

## Status

`done` — `cargo test -p kerness tooling` passes 6 tests, `cargo test -p kerness
toolkit` passes 6, `cargo test -p kerness --test tools_e2e` passes 18, and the
two Python modules pass 3 and 7.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/tooling.rs` | `ToolSpec`, `ToolCall`, `ToolHandler`, `Arguments`, the fence parser, prompt rendering |
| `crates/kerness/src/toolkit.rs` | `ToolDispatcher`, `ToolResult`, `resolve` |
| `bindings/python/src/types.rs:70,165,179,294` | `PyToolCall`, `PyToolHandler`, `PyToolSpec`, `PyToolResult` |
| `bindings/python/src/runtime.rs:161` | `PyToolDispatcher` |
| `bindings/python/src/funcs.rs:104,407` | `parse_tool_calls`, `format_tools_prompt`, `resolve` as pyfunctions; `INVALID_CALL` at `:682` |
| `bindings/python/kerness/{tooling,toolkit}.py` | re-export shims |

## Language and Conventions

Two Rust crate modules, pyclasses in the types and runtime binding modules, and
two Python re-export shims; the root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- `ToolSpec` implements `PartialEq` (`crates/kerness/src/tooling.rs:87`) and
  `Debug` (`:97`) by hand because the handler is a trait object: equality
  compares name, description, schema, actor flag and handler identity
  (`Arc::ptr_eq`); `Debug` prints the metadata and `finish_non_exhaustive`.
- `ToolCall` and `ToolResult` derive `Serialize`/`Deserialize`
  (`crates/kerness/src/tooling.rs:110`, `crates/kerness/src/toolkit.rs:18`)
  because both travel in the run checkpoint; neither is
  `deny_unknown_fields`.
- The fence regex is a `LazyLock<Regex>` with `expect("static pattern")`
  (`crates/kerness/src/tooling.rs:145`). The `tool_calls` key test is a
  hand-written byte scan (`:268`) because it needs a lookbehind the `regex`
  crate does not have.
- Dispatch never returns `Result`: every failure is a `ToolResult` with
  `is_error` set (`crates/kerness/src/toolkit.rs:66`). Handlers do return
  `crate::error::Result<String>` (`crates/kerness/src/tooling.rs:32`).
- `PyToolCall`, `PyToolHandler`, `PyToolSpec` and `PyToolResult` are `frozen`
  pyclasses; `PyToolResult` is `get_all` (`bindings/python/src/types.rs:294`).
- Tests are inline with no fixtures beyond a `spec_named` helper; the Python
  tests group by outcome (`TestSuccess`, `TestFailuresBecomeResults`,
  `TestResolve`).

## Design and Invariants

**Dependency direction.** `tooling` imports `error` and `pyfmt`; `toolkit`
imports `jsonschema`, `pyfmt` and `tooling`. Neither imports `toolschema`,
`provider` or `session`; the native dialects sit above this module and consume
its types. Callers: [agent-runtime.md](agent-runtime.md) parses text replies
(`crates/kerness/src/agent_runtime.rs:477`), [prompting.md](prompting.md)
renders the prompt block (`crates/kerness/src/prompting.rs:247`), and
[session.md](session.md) composes `resolve` twice in `Shared::active_tools`
(`crates/kerness/src/session.rs:459`, `:464`).

**The parser is a recovery layer, not a wire decoder.** A ```` ```json ````
fence and a bare object both count, but only when the payload declares the
exact `tool_calls` key (`crates/kerness/src/tooling.rs:161`); without that
test, the ```` ```json ```` result block a gameplan's `result:` shape asks for
would parse as a malformed call and the model would be asked for the same
closing summary indefinitely. A malformed batch comes back as one
`ToolCall::invalid(...)` carrying the reason rather than vanishing, because a
batch that vanishes costs a turn while one that comes back as an error costs a
tool call. Unparseable arguments are preserved under `raw` (`decode_arguments`,
`:240`) so the model can see what it sent. Tests:
`a_mislabeled_json_fence_counts_only_when_it_mentions_tool_calls` (`:382`),
`a_bare_object_counts_only_with_the_exact_key` (`:394`),
`malformed_batches_come_back_as_readable_errors` (`:403`),
`unparseable_arguments_are_preserved_under_raw` (`:425`).

**Every failure is a result the model can read.** `execute` answers an invalid
batch, an unknown tool, a schema violation and a handler error each as a
`ToolResult` with `is_error`; it never propagates, because a failing tool is
information for the model, not a failed session
(`crates/kerness/src/toolkit.rs:66`). Arguments are checked by
`validate_arguments` (`:87`) against the same schema the model was shown.
Tests: `every_failure_is_a_result_the_model_can_read` (`:168`),
`arguments_are_checked_against_the_schema` (`:191`), and through a session in
`crates/kerness/tests/tools_e2e.rs`.

**The actor reaches only handlers that asked for it.** `takes_actor`
(`crates/kerness/src/tooling.rs:61`) is set by the built-in `cmd`, `read_file`
and `list_dir` tools because access prompts and the command log name the actor;
a handler that did not opt in receives `""` (`crates/kerness/src/toolkit.rs:92`).
Tests: `the_actor_reaches_only_handlers_that_asked_for_it` (`:145`) and
`bindings/python/tests/test_toolkit.py:38`.

**The dispatcher reads its tool list per call.** `ToolsFor`
(`crates/kerness/src/toolkit.rs:49`) is a closure, not a captured list, so the
harness narrowing resolved during `run()` and the per-turn skill gate are both
picked up. A tool the agent was not offered is not callable if the model asks
anyway — narrowing binds dispatch as well as the prompt. No unit test enforces
the per-call read; `crates/kerness/tests/tools_e2e.rs` proves it through a
session whose second call is offered a different set.

**`None` is everything, `[]` is nothing, order is registration.** `resolve`
(`crates/kerness/src/toolkit.rs:106`) keeps registration order rather than
allow-list order so the prompt's tool block does not depend on how the caller
typed it. Test: `absent_is_everything_empty_is_nothing_and_order_is_registration`
(`:210`) and `bindings/python/tests/test_toolkit.py:108`.

**The catalog is whole, and narrowing is the lever.** Every tool a turn is
offered is described in full on that turn — as native schemas in the request
body, or under `ToolDialect::Text` as `format_tools_prompt` output inside the
system message. There is no summary catalog the model expands. That cost is paid
per turn, and the framework's answer is to make the set smaller: a gameplan's
`tools:` ([harness.md](harness.md)), an agent's own `tools`
([agent.md](agent.md)), a skill's `allowed-tools:` ([skills.md](skills.md)), and
the access policy's `allowed_commands` ([access.md](access.md)) for what
`run_command` can reach. `resolve` is the shared body for the first two, and
`Shared::active_tools` in [session.md](session.md) is the order they compose in.

## Key Types and Entry Points

- `crates/kerness/src/tooling.rs:32` — `ToolHandler` — `call(arguments, actor)
  -> Result<String>`; blanket-implemented for any
  `Fn(&Arguments, &str) -> Result<String> + Send + Sync`, which is how a Rust
  closure and a Python callable both satisfy it.
- `crates/kerness/src/tooling.rs:47` — `ToolSpec` — name, description,
  parameter schema, `Arc<dyn ToolHandler>`, and `takes_actor`; `with_actor()` at
  `:81` opts a handler in.
- `crates/kerness/src/tooling.rs:110` — `ToolCall` — name, `Arguments`, and the
  provider's correlation `id`; `invalid(error)` at `:133` builds the call that
  carries a parse error under the reserved name `INVALID_CALL` (`:26`).
- `crates/kerness/src/tooling.rs:161` — `parse_tool_calls(text)` — a single-pass
  fence scan over a reply; returns every call it finds, including invalid ones,
  and an empty vector when the text holds no call.
- `crates/kerness/src/tooling.rs:305` — `extract_fenced_json(text, fence_names)`
  — the first fenced block with one of the given labels, `""` for none, or the
  unclosed sentinel; shared with the session's result parsing.
- `crates/kerness/src/tooling.rs:331` — `format_tools_prompt(tools)` — the tool
  instructions and definitions for a model without native tool support; empty
  string for no tools.
- `crates/kerness/src/toolkit.rs:18` — `ToolResult` — name, content, `is_error`;
  what the model reads next.
- `crates/kerness/src/toolkit.rs:52` — `ToolDispatcher` — holds a `ToolsFor`
  closure; `execute(call, actor)` at `:66` validates, calls, and shapes the
  result without ever returning an error.
- `crates/kerness/src/toolkit.rs:106` — `resolve(tools, allowed)` — the
  allow-list narrowing shared by the gameplan and per-agent lists.
- `bindings/python/src/types.rs:165` — `PyToolHandler` — a Rust closure seen from
  Python as a callable, so `spec.handler(...)` works for the built-in and `Skill`
  tools; `bindings/python/src/runtime.rs:161` — `PyToolDispatcher`, whose
  `tools_for` lookup yields no tools if the Python callable raises.

## Interactions

- Dispatched during a turn by [agent-runtime.md](agent-runtime.md), which parses
  a text reply with `parse_tool_calls` and hands each pending call to `execute`.
- [run.md](run.md) owns scoped tool capabilities, approval preflight, durable
  invocation state and reconciliation around this dispatcher; `ToolCall` and
  `ToolResult` are the serialized shapes in its continuation.
- Arguments are validated by [jsonschema.md](jsonschema.md)'s
  `validate_arguments`; its messages go back to the model verbatim.
- Native tool definitions are built from `ToolSpec` by
  [toolschema.md](toolschema.md); the prompt fallback is here.
- Narrowed by [skills.md](skills.md)'s `apply_gate` and widened by its
  `admit_required`, both after `resolve`; the `Skill` tool is itself a
  `ToolSpec`.
- The built-in `run_command`, `read_file` and `list_dir` handlers go through
  [access.md](access.md) and set `takes_actor`.
- [harness.md](harness.md) resolves a gameplan's `tools:` against the
  registered names; `RESERVED_TOOL_NAMES` keeps `Skill` off the registry.

## How to Test

```sh
cargo test -p kerness tooling                                       # pass = 6 passed, 0 failed
cargo test -p kerness toolkit                                       # pass = 6 passed, 0 failed
cargo test -p kerness --test tools_e2e                              # pass = 18 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_tooling.py -q # pass = 3 passed
.venv/bin/python -m pytest bindings/python/tests/test_toolkit.py -q # pass = 7 passed
```

- `bindings/python/tests/test_tooling.py:12` —
  `test_a_call_is_found_in_every_wrapper_a_model_reaches_for` — the parser's
  real job: models fence a call half a dozen different ways.
- `bindings/python/tests/test_tooling.py:36`
  `test_output_that_merely_looks_like_one_is_left_alone` and `:46`
  `test_a_payload_with_nothing_callable_in_it_becomes_an_invalid_call` — the
  two failure directions, neither of which raises.
- `bindings/python/tests/test_toolkit.py:108` —
  `test_absent_is_everything_empty_is_nothing_and_order_is_registration` —
  `resolve`'s three-way distinction.
- `bindings/python/tests/test_toolkit.py:89` — `test_a_raising_handler` — a
  handler that raises becomes a `ToolResult` carrying the error rather than
  propagating; `:97` `test_a_denied_command_is_reported_not_raised` — the same
  for an access refusal; `:38` — the actor reaches only the handlers that asked
  for it.
- `bindings/python/tests/test_skill_runtime.py:120` calls
  `spec.handler({"name": "a"})` directly — the `Skill` tool's handler is a Rust
  closure, so this is what proves `PyToolHandler`
  ([bindings.md](bindings.md)) makes it callable from Python.
- `crates/kerness/tests/tools_e2e.rs` — the loop inside a real turn in all
  three dialects; unknown tool, schema violation and failing handler answered as
  text; `MAX_INVALID_CALLS`; `max_tool_iterations`; an agent's own `tools`
  narrowing what it is offered and a tool it gave up refused at dispatch.
- Gaps: no unit test drives a `tools_for` Python callable that raises
  (`bindings/python/src/runtime.rs:161`); the "yields no tools" behaviour is
  observed in code only. `format_tools_prompt`'s instruction text is asserted
  only for the empty case (`crates/kerness/src/tooling.rs:432`).

## Review and Refactor Guide

- Changing the text protocol (fence label, JSON shape, the instruction lines) →
  `parse_tool_calls`, `extract_fenced_json` and `format_tools_prompt` together,
  because the prompt teaches what the parser reads; then
  `bindings/python/tests/test_tooling.py:12` and `tools_e2e.rs` under `ToolDialect::Text`. `extract_fenced_json` is also
  used by the session's closing-result parse, so a change to its sentinel
  semantics reaches [loop.md](loop.md).
- Changing `ToolResult` or `ToolCall` fields → both are serialized in the run
  continuation; check `crates/kerness/tests/resume.rs` and the Python
  `get_all` surface of `PyToolResult`.
- Changing the failure wording in `execute` → models read it; `tools_e2e.rs`
  and `test_toolkit.py` assert substrings.
- Adding a built-in tool → register it beside `cmd`/`read_file`/`list_dir` in
  the session's default tool list, route it through `AccessManager`, set
  `takes_actor` if it logs or prompts, and add it to the reserved or
  duplicate-name rules only if it must be.
- Safe extension points: a new caller-supplied tool is `Session::add_tool` or
  `add_contextual_tool`; nothing here changes. A new narrowing lever composes in
  `Shared::active_tools`, not here.
- Forbidden coupling: `tooling`/`toolkit` must not import `toolschema`,
  `provider` or `session`; the dialect layer consumes `ToolSpec`, not the other
  way round.
- Compatibility: `INVALID_CALL` is exported to Python
  (`bindings/python/src/funcs.rs:682`) and appears in checkpoints; renaming it
  is a schema change.

Improvement candidates (proposals, not accepted work):

- Assert `format_tools_prompt`'s instruction block once against a literal, the
  way `pyfmt` is; success check: a `tooling.rs` case that fails when a line the
  models are trained on is edited by accident.
- A per-tool timeout at dispatch, so a slow caller-supplied handler cannot hold
  a synchronous turn indefinitely; success check: a `tools_e2e.rs` case with a
  sleeping handler ending as an error result within the bound. Requires a
  cooperative contract, since the handler runs on the calling thread.

## Open Gaps / Roadmap

- `parse_tool_calls` recognises the framework's fenced-block convention only. A
  model that invents a different format produces no calls, not an error.
- Tool results are strings. A tool returning structured data has to serialise
  it, and the model has to parse it back.
- No per-tool timeout; only `run_command` bounds its own execution
  ([access.md](access.md)).
- The catalog does not scale past what narrowing can reach. Four subtractive
  levers are enough for a session whose tools are registered by its host
  program, because whoever registers them knows which agent needs which. A tool
  source the host program did not enumerate — the MCP client on the root
  roadmap (M4) — would put that knowledge outside the session, and a summary
  catalog the model expands on demand is the shape that answers it. Building it
  before there is such a source would be an abstraction with one caller.
