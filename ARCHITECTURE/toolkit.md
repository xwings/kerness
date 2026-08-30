# Toolkit

## Goal

Tools, end to end: what a tool is (`ToolSpec`), how a call is recognised in a
model's reply (`parse_tool_calls`), how tools are described to a model that has
no native tool support (`format_tools_prompt`), and how a call is executed and
its result shaped (`ToolDispatcher`).

`tooling.rs` owns the data and the text parsing; `toolkit.rs` owns dispatch.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/tooling.rs` | `ToolSpec`, `ToolCall`, `ToolHandler`, parsing, prompt rendering |
| `crates/kerness/src/toolkit.rs` | `ToolDispatcher`, `ToolResult`, `resolve` |
| `bindings/python/src/types.rs:68,165,179,294` | `PyToolCall`, `PyToolHandler`, `PyToolSpec`, `PyToolResult` |
| `bindings/python/src/runtime.rs:161` | `PyToolDispatcher` |
| `bindings/python/kerness/{tooling,toolkit}.py` | re-export shims |

## Key Types and Entry Points

- `crates/kerness/src/tooling.rs:32` — `ToolHandler` — `call(arguments, actor)`;
  the trait a Rust closure or a Python callable both satisfy.
- `crates/kerness/src/tooling.rs:47` — `ToolSpec` — name, description, parameter
  schema, handler, and whether the handler wants the actor name (`with_actor` at
  `:81`).
- `crates/kerness/src/tooling.rs:110` — `ToolCall` — name and arguments; `invalid`
  at `:133` constructs the call that carries a parse error instead.
- `crates/kerness/src/tooling.rs:26` — `INVALID_CALL` — the sentinel name an
  invalid call takes, so the dispatcher can recognise it without a separate type.
- `crates/kerness/src/tooling.rs:161` — `parse_tool_calls(text)` — a single-pass
  fence scanner over the reply; returns every call it finds, including invalid
  ones.
- `crates/kerness/src/tooling.rs:331` — `format_tools_prompt(tools)` — the tool
  instructions for a model without native tool support.
- `crates/kerness/src/toolkit.rs:54` — `ToolDispatcher` — holds a `ToolsFor`
  closure rather than a tool list, so the available tools can change per turn as
  skills activate.
- `crates/kerness/src/toolkit.rs:68` — `execute(call, actor)` — validates
  arguments, calls the handler, and turns any error into a `ToolResult` the model
  can read. It does not propagate: a failing tool is information for the model,
  not a failed session.
- `crates/kerness/src/toolkit.rs:108` — `resolve(tools, allowed)` — the
  allowed-list narrowing, shared with the skill gate.

`ToolSpec` implements `PartialEq` (`tooling.rs:87`) and `Debug` (`:97`) by hand,
because the handler is a trait object: equality is by name and schema, and
`Debug` prints the name rather than nothing.

### The catalog is whole, and narrowing is the lever

Every tool a turn is offered is described in full on that turn — as native
schemas in the request body, or, under `ToolDialect::Text`, as
`format_tools_prompt` output inside the system message. Nothing is disclosed
lazily: there is no summary catalog the model expands, and no tool it has to ask
about before it can call it.

That is a cost, and it is paid per turn. A session with thirty registered tools
writes thirty descriptions on every call an agent makes, whether it needs two of
them or none. The framework's answer is to make the set smaller rather than the
description shorter, and there are four places to do it, all of them
subtractive: a gameplan's `tools:` ([harness.md](harness.md)), an agent's own
`tools` ([agent.md](agent.md)), a skill's `allowed-tools:`
([skills.md](skills.md)), and the access policy's `allowed_commands`
([access.md](access.md)) for what `run_command` can reach. `resolve`
(`toolkit.rs:108`) is the shared body for the first three; `Shared::active_tools`
in [session.md](session.md) is the order they compose in.

Narrowing beats lazy disclosure here because it binds the dispatcher as well as
the prompt. A tool an agent was not offered is not callable if the model asks
for it anyway, which a hidden-but-present tool would still be — and a model that
cannot see a tool it must not use is also a model that cannot be argued into
using it.

## Interactions

- Dispatched during a turn by [agent-runtime.md](agent-runtime.md).
- Arguments validated by [jsonschema.md](jsonschema.md).
- Native tool definitions built by [toolschema.md](toolschema.md); the prompt
  fallback is here.
- Narrowed by [skills.md](skills.md)'s gate, and the `Skill` tool is itself a
  `ToolSpec`.
- The built-in `run_command`, `read_file`, and `list_dir` handlers go through
  [access.md](access.md).

## How to Test

```sh
cargo test -p kerness tooling                                       # pass = 0 failed
cargo test -p kerness toolkit                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_tooling.py -q # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_toolkit.py -q # pass = 0 failed
```

- `bindings/python/tests/test_tooling.py:12` — `test_a_call_is_found_in_every_wrapper_a_model_reaches_for` —
  the parser's real job: models fence a call half a dozen different ways.
- `:36` `test_output_that_merely_looks_like_one_is_left_alone` and `:46`
  `test_a_payload_with_nothing_callable_in_it_becomes_an_invalid_call` — the two
  failure directions, neither of which raises.
- `bindings/python/tests/test_toolkit.py:108` — `test_absent_is_everything_empty_is_nothing_and_order_is_registration` —
  `resolve`'s three-way distinction.
- `bindings/python/tests/test_toolkit.py:93` asserts a handler that raises becomes a `ToolResult`
  carrying the error rather than propagating, and `:38` that the actor reaches
  only the handlers that asked for it.
- `bindings/python/tests/test_skill_runtime.py:120` calls `spec.handler({"name": "a"})` directly —
  the `Skill` tool's handler is a Rust closure, so this is what proves
  `PyToolHandler` ([bindings.md](bindings.md)) makes it callable from Python.

## Open Gaps / Roadmap

- `parse_tool_calls` recognises the framework's fenced-block convention only. A
  model that invents a different format produces no calls, not an error.
- Tool results are strings. A tool returning structured data has to serialise it,
  and the model has to parse it back.
- No per-tool timeout; only `run_command` bounds its own execution
  ([access.md](access.md)).
- The catalog does not scale past what narrowing can reach. Four subtractive
  levers are enough for a session whose tools are registered by its host
  program, because whoever registers them knows which agent needs which. A tool
  source the host program did not enumerate — the MCP client on the root
  roadmap — would put that knowledge outside the session, and a summary catalog
  the model expands on demand is the shape that answers it. Building it before
  there is such a source would be an abstraction with one caller.
