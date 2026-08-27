# Toolkit

## Goal

Tools, end to end: what a tool is (`ToolSpec`), how a call is recognised in a
model's reply (`parse_tool_calls`), how tools are described to a model that has
no native tool support (`format_tools_prompt`), and how a call is executed and
its result shaped (`ToolDispatcher`). Serves **M1**.

`tooling.rs` owns the data and the text parsing; `toolkit.rs` owns dispatch.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/tooling.rs` | `ToolSpec`, `ToolCall`, `ToolHandler`, parsing, prompt rendering |
| `crates/kerness/src/toolkit.rs` | `ToolDispatcher`, `ToolResult`, `resolve` |
| `crates/kerness-py/src/types.rs:65,162,176,291` | `PyToolCall`, `PyToolHandler`, `PyToolSpec`, `PyToolResult` |
| `crates/kerness-py/src/runtime.rs:172` | `PyToolDispatcher` |
| `python/kerness/{tooling,toolkit}.py` | re-export shims |

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
- `crates/kerness/src/tooling.rs:329` — `format_tools_prompt(tools)` — the tool
  instructions for a model without native tool support.
- `crates/kerness/src/toolkit.rs:54` — `ToolDispatcher` — holds a `ToolsFor`
  closure rather than a tool list, so the available tools can change per turn as
  skills activate.
- `crates/kerness/src/toolkit.rs:68` — `execute(call, actor)` — validates
  arguments, calls the handler, and turns any error into a `ToolResult` the model
  can read. It does not propagate: a failing tool is information for the model,
  not a failed session.
- `crates/kerness/src/toolkit.rs:111` — `resolve(tools, allowed)` — the
  allowed-list narrowing, shared with the skill gate.

`ToolSpec` implements `PartialEq` (`tooling.rs:87`) and `Debug` (`:97`) by hand,
because the handler is a trait object: equality is by name and schema, and
`Debug` prints the name rather than nothing.

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
cargo test -p kerness tooling                        # pass = 0 failed
cargo test -p kerness toolkit                        # pass = 0 failed
.venv/bin/python -m pytest tests/test_tooling.py -q  # pass = 0 failed
.venv/bin/python -m pytest tests/test_toolkit.py -q  # pass = 0 failed
```

- `tests/test_tooling.py:12` — `test_a_call_is_found_in_every_wrapper_a_model_reaches_for` —
  the parser's real job: models fence a call half a dozen different ways.
- `:36` `test_output_that_merely_looks_like_one_is_left_alone` and `:46`
  `test_a_payload_with_nothing_callable_in_it_becomes_an_invalid_call` — the two
  failure directions, neither of which raises.
- `tests/test_toolkit.py:108` — `test_absent_is_everything_empty_is_nothing_and_order_is_registration` —
  `resolve`'s three-way distinction.
- `tests/test_toolkit.py:93` asserts a handler that raises becomes a `ToolResult`
  carrying the error rather than propagating, and `:38` that the actor reaches
  only the handlers that asked for it.
- `tests/test_skill_runtime.py:119` calls `spec.handler({"name": "a"})` directly —
  the `Skill` tool's handler is a Rust closure, so this is what proves
  `PyToolHandler` ([bindings.md](bindings.md)) makes it callable from Python.

## Open Gaps / Roadmap

- `parse_tool_calls` recognises the framework's fenced-block convention only. A
  model that invents a different format produces no calls, not an error.
- Tool results are strings. A tool returning structured data has to serialise it,
  and the model has to parse it back.
- No per-tool timeout; only `run_command` bounds its own execution
  ([access.md](access.md)).
