# Tool Schema

## Goal

Native tool calling, in three dialects. Providers differ in how tools are
declared, how a tool call comes back, and how a result is fed in; this module
owns all three shapes for each. When a provider has no native support the
framework falls back to the prompt rendering in [toolkit.md](toolkit.md), and
`ToolDialect` is what decides between them.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/toolschema.rs` | `ToolDialect` and the per-dialect conversions |
| `bindings/python/src/types.rs:38` | `register_dialect`, and dialect conversion both ways |
| `bindings/python/kerness/_enums.py:14` | `ToolDialect` as a real `enum.Enum` |
| `bindings/python/kerness/toolschema.py` | re-export shim |

`ToolDialect` is declared in Python rather than as a pyclass because callers
compare members with `is`, which requires genuine enum member identity. The
extension is handed the class at bootstrap and converts across the boundary by
value.

## Key Types and Entry Points

- `crates/kerness/src/toolschema.rs:37` — `ToolDialect` — the dialect, including
  the "no native tools" case that selects the prompt fallback.
- `crates/kerness/src/toolschema.rs:55` — `parse(value)` — returns `Option`; an
  unknown dialect string is not an error here, it is "not this one".
- `crates/kerness/src/toolschema.rs:72` — `to_openai_tool(spec)` / `:84`
  `to_anthropic_tool(spec)` — one `ToolSpec` in each wire shape.
- `crates/kerness/src/toolschema.rs:96` — `tool_schemas(dialect, tools)` — the
  whole list, or `None` when the dialect has no native tools; the `None` is what
  tells the caller to render a prompt instead.
- `crates/kerness/src/toolschema.rs:111` — `parse_openai_tool_calls(message)` /
  `:136` `parse_anthropic_tool_calls(response)` — calls out of a native response.
- `crates/kerness/src/toolschema.rs:214` — `render_assistant_turn(dialect, response)` —
  the assistant message to append, which must echo the tool calls in the shape the
  provider expects to see them again.
- `crates/kerness/src/toolschema.rs:256` — `render_tool_result(dialect, call, result)` —
  the result message, which is where the dialects differ most: a `tool` role
  message versus a user message carrying a `tool_result` block.

## Interactions

- Selected per agent by [provider.md](provider.md)'s `effective_dialect`.
- Consumes the `ToolSpec` list from [toolkit.md](toolkit.md) and produces the
  calls it dispatches.
- Schemas are made strict by [jsonschema.md](jsonschema.md).
- The dialect is branched on in [prompting.md](prompting.md), which omits tool
  instructions when they are native.

## How to Test

```sh
cargo test -p kerness toolschema                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_toolschema.py -q # pass = 0 failed
```

- `bindings/python/tests/test_toolschema.py` asserts exact wire shapes for each dialect in both
  directions — `test_openai_uses_a_tool_role_message` (`:133`) against
  `test_anthropic_uses_a_user_message_carrying_the_error_flag_natively` (`:142`)
  is the pair that shows how far the dialects diverge.
- `bindings/python/tests/test_toolschema.py:52` — `tool_schemas(ToolDialect.TEXT, [CMD]) is None`
  and `tool_schemas(ToolDialect.OPENAI, []) is None`: no native tools and no tools
  are both `None`, not an empty list.
- `bindings/python/tests/test_provider.py:479` onward asserts `effective_dialect() is ToolDialect.OPENAI`
  — identity, not equality. That is what requires the enum to be a real
  `enum.Enum` on the Python side rather than a pyclass.

## Open Gaps / Roadmap

- Three dialects cover the four shipped providers. A `CustomProvider` against an
  endpoint with a fourth shape has to use the prompt fallback.
- Streaming tool calls are not parsed, because responses are not streamed
  ([provider.md](provider.md)).
