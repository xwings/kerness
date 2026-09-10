---
eatmycode_version: "1.2.0"
---

# Tool Schema

## Goal

Native tool calling, in three dialects. Providers differ in how tools are
declared, how a tool call comes back, and how a result is fed in; this module
owns all three shapes for each. When a provider has no native support the
framework falls back to the prompt rendering in [toolkit.md](toolkit.md), and
`ToolDialect` is what decides between them.

It owns the wire shapes and nothing above them: which dialect an agent speaks is
[provider.md](provider.md)'s `effective_dialect`; when the assistant turn and the
result are appended is [agent-runtime.md](agent-runtime.md)'s; the `ToolSpec` it
converts and the `ToolCall`/`ToolResult` it produces and renders belong to
[toolkit.md](toolkit.md).

## Status

`done` — `cargo test -p kerness toolschema` passes 8 tests and
`bindings/python/tests/test_toolschema.py` passes 12.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/toolschema.rs` | `ToolDialect` and the per-dialect conversions |
| `bindings/python/src/types.rs:40` | `register_dialect`, `dialect_to_py`, `dialect_from_py` |
| `bindings/python/src/funcs.rs:125` | the seven conversion pyfunctions |
| `bindings/python/kerness/_enums.py:14` | `ToolDialect` as a real `enum.Enum` |
| `bindings/python/kerness/toolschema.py` | re-export shim |

## Language and Conventions

One Rust crate module, conversion helpers in the types and funcs binding
modules, and a Python enum plus shim; the root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply. Local facts:

- `ToolDialect` derives `Serialize`/`Deserialize` with
  `rename_all = "snake_case"` (`crates/kerness/src/toolschema.rs:35`), so its
  checkpoint spelling, `as_str` (`:44`) and `parse` (`:53`) agree; `Default`
  is `Text`. Providers choose the dialect; harness frontmatter has no dialect key.
- The module doc carries the dialect comparison table
  (`crates/kerness/src/toolschema.rs:1`); the Python shim's docstring
  summarises it in prose. Observed convention: a wire-shape fact is documented at the converter.
- `ToolDialect` is declared in Python (`bindings/python/kerness/_enums.py:14`)
  rather than as a pyclass because callers compare members with `is`, which
  requires genuine enum member identity. The extension is handed the class at
  `bootstrap` (`bindings/python/src/lib.rs:42`) and converts by value:
  `dialect_to_py` calls the enum with the wire string
  (`bindings/python/src/types.rs:47`), and `dialect_from_py` accepts a member or
  its bare value (`:55`).
- All conversion functions are pure and return `Value`, `Option<Vec<Value>>`
  or `Vec<ToolCall>`, never `Result`; a malformed input is preserved, not refused. Tests are inline
  with a single `spec()` helper; the Python tests group by direction
  (`TestConversion`, `TestParsingOpenAI`, `TestRenderingResults`).

## Design and Invariants

**Dependency direction.** `toolschema` imports `provider` (for
`ProviderResponse`), `pyfmt`, `tooling` and `toolkit`; nothing in it imports
`session`. Its callers are the four backends
(`crates/kerness/src/provider/mod.rs:667` attaches schemas, `:574` and
`crates/kerness/src/provider/claude.rs:166` parse calls), the agent runtime
(`crates/kerness/src/agent_runtime.rs:237`, `:255` render the two messages),
the prompt assembler (`crates/kerness/src/prompting.rs:243` drops the prose
tools block for a native dialect) and the session's overhead estimate
(`crates/kerness/src/session.rs:1722`).

**One converter per dialect; nothing above is dialect-aware.** Rather than a
lowest common denominator, each direction has a function per dialect and the
runtime calls them with the agent's dialect. The differences are concrete: the
schema is nested under `function` for OpenAI and flat for Anthropic; the key is
`parameters` versus `input_schema`; a call arrives in `message.tool_calls[]`
versus a `tool_use` content block; arguments are a JSON **string** versus a
JSON **object**; a result goes in a `role: "tool"` message versus a
`tool_result` block inside a `role: "user"` message. Test:
`each_dialect_puts_the_schema_where_its_api_expects_it`
(`crates/kerness/src/toolschema.rs:291`).

**No tools means no `tools` key.** `tool_schemas` answers `None` for the
`Text` dialect and for an empty tool set (`crates/kerness/src/toolschema.rs:94`);
an empty list would become `tools: []`, which OpenAI rejects. Tests:
`a_request_with_no_tools_to_send_carries_no_tools_key` (`:305`) and
`test_nothing_is_sent_under_text_or_with_no_tools`
(`bindings/python/tests/test_toolschema.py:50`).

**Malformed arguments are preserved, not dropped.** An OpenAI argument string
that is not JSON is kept under `raw` (`decode_arguments`,
`crates/kerness/src/toolschema.rs:183`) so the
dispatcher reports a schema error the model can correct instead of the call
vanishing; an Anthropic `input` that is not an object is wrapped the same way
(`:150`). This decoder is deliberately distinct from the text-fence decoder in
`tooling.rs`: a native call that sends neither an object nor a string sent no
arguments, whereas the fence protocol preserves whatever the model typed. Both
share `tooling::wrap_raw`. Tests:
`malformed_openai_arguments_are_preserved_rather_than_dropped` (`:325`),
`anthropic_arguments_arrive_as_an_object_and_other_blocks_are_ignored` (`:335`).

**The assistant turn is replayed in each API's own shape.** Both native APIs
return 400 if the turn that made the calls is not replayed before its results.
`render_assistant_turn` (`crates/kerness/src/toolschema.rs:202`) re-encodes
OpenAI arguments as a string and
Anthropic ones as `tool_use` blocks, and sends `null` rather than `""` for a
content-free OpenAI turn and no text block at all for Anthropic, because an
empty text block is also a 400. Tests:
`the_assistant_turn_is_replayed_in_each_apis_own_shape` (`:349`),
`a_content_free_tool_turn_sends_null_rather_than_an_empty_string` (`:379`).

**Only dialects without an error flag mark failures inline.** Anthropic's
`tool_result` carries `is_error` natively, so its content stays clean; OpenAI
and Text get `[ToolError] ` prepended by `error_text`
(`crates/kerness/src/toolschema.rs:268`), and Text also
gets the `[Tool:name] ` prefix. Tests:
`only_the_dialects_without_an_error_flag_mark_failures_inline` (`:399`) and
`test_anthropic_uses_a_user_message_carrying_the_error_flag_natively`
(`bindings/python/tests/test_toolschema.py:142`).

**Identity across the boundary.** `provider.effective_dialect() is
ToolDialect.OPENAI` must hold in Python
(`bindings/python/tests/test_provider.py:483`, `:501`); that is what forces the
enum to live in Python and the extension to hand back the same member object
rather than a reconstructed value.

## Key Types and Entry Points

- `crates/kerness/src/toolschema.rs:35` — `ToolDialect` — `Text | Openai |
  Anthropic`, `Text` by default; `as_str()` at `:44` is the serialized
  name, `parse(value)` at `:53` returns `Option` because an unknown string is
  "not this one", not an error.
- `crates/kerness/src/toolschema.rs:70` — `to_openai_tool(spec)` / `:82`
  `to_anthropic_tool(spec)` — one `ToolSpec` in each wire shape.
- `crates/kerness/src/toolschema.rs:94` — `tool_schemas(dialect, tools)` — the
  whole list, or `None` when the dialect has no native tools or the list is
  empty; the `None` is what tells the caller to send no `tools` key.
- `crates/kerness/src/toolschema.rs:109` — `parse_openai_tool_calls(message)` —
  calls out of a `choices[].message`; a call with no name is skipped, a missing
  id stays empty.
- `crates/kerness/src/toolschema.rs:134` — `parse_anthropic_tool_calls(response)`
  — calls out of the `content` blocks whose `type` is `tool_use`; other blocks
  are ignored.
- `crates/kerness/src/toolschema.rs:202` — `render_assistant_turn(dialect,
  response)` — the assistant message to append, echoing the calls in the shape
  the provider expects to see them again.
- `crates/kerness/src/toolschema.rs:244` — `render_tool_result(dialect, call,
  result)` — the result message the model reads next; the dialects differ most
  here.
- `bindings/python/src/types.rs:40` — `register_dialect(class)`, with
  `dialect_to_py` and `dialect_from_py` below it — the by-value crossing that
  preserves member identity; an unknown value is `ValueError`.
- `bindings/python/src/funcs.rs:125` — `to_openai_tool` through
  `render_tool_result` (`:193`) — the seven pyfunctions, each a direct forward.

## Interactions

- Selected per agent by [provider.md](provider.md)'s `effective_dialect`
  (`crates/kerness/src/provider/mod.rs:250`, body at `:318`); the
  `note_native_tools_rejected` latch (`:269`) drops a provider to `Text` after a
  refused request, and every function here follows that dialect.
- Consumes the `ToolSpec` list from [toolkit.md](toolkit.md) and produces the
  `ToolCall`s it dispatches; renders the `ToolResult` it returns. Both parsers
  use `tooling::wrap_raw`; native and text-fence argument decoding keep their
  distinct rules.
- Tool schemas retain `ToolSpec.parameters` unchanged when attached to a
  request (`crates/kerness/src/toolschema.rs:70`, `:82`). Strict rewriting by
  [jsonschema.md](jsonschema.md) applies to OpenAI structured output schemas
  (`crates/kerness/src/provider/openai.rs:88`), a separate request contract.
- [agent-runtime.md](agent-runtime.md) appends the two rendered messages to the
  turn's private history; under a native dialect the batch reaches the next
  request whole.
- The dialect is branched on in [prompting.md](prompting.md), which omits the
  prose tool instructions when tools are native, and in
  [compaction.md](compaction.md)'s overhead estimate, which counts native
  schemas as request-body overhead.
- Integration through a session in all three dialects is
  `crates/kerness/tests/tools_e2e.rs`, driven by the `ToolProvider` double.

## How to Test

```sh
cargo test -p kerness toolschema                                       # pass = 8 passed, 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_toolschema.py -q # pass = 12 passed
```

- `bindings/python/tests/test_toolschema.py:133` —
  `test_openai_uses_a_tool_role_message` — against `:142`
  `test_anthropic_uses_a_user_message_carrying_the_error_flag_natively`: the
  pair that shows how far the dialects diverge; `:163`
  `test_dialects_without_an_error_flag_mark_it_inline` and `:169`
  `test_text_rendering_is_unchanged_from_before_native_calling` pin the Text
  shape.
- `bindings/python/tests/test_toolschema.py:50` —
  `test_nothing_is_sent_under_text_or_with_no_tools`: no native tools and no
  tools are both `None`, not an empty list.
- `bindings/python/tests/test_toolschema.py:57`, `:78`, `:87` — argument
  decoding in both native dialects, including the malformed-string case kept
  under `raw`; `:104`, `:113`, `:125` — the assistant-turn replay per dialect.
- `bindings/python/tests/test_provider.py:483` and `:501` assert
  `effective_dialect() is ToolDialect.OPENAI` / `TEXT` — identity, not
  equality, which is what requires the enum to be a real `enum.Enum` on the
  Python side.
- Gaps: no test drives `dialect_from_py` with an unknown string
  (`bindings/python/src/types.rs:55`); the `ValueError` path is observed in code
  only.

## Review and Refactor Guide

- Changing a wire shape → the converter for that dialect and the inline test
  that pins it, then `tools_e2e.rs` under that dialect; the Python test asserts
  the exact dictionary and must change in the same commit.
- Adding a dialect → the enum variant, `as_str`/`parse`, a branch in each of
  `tool_schemas`, `render_assistant_turn` and `render_tool_result`, a parser,
  the Python enum member in `_enums.py`, and the backend that selects it in
  [provider.md](provider.md). The `_` arm in `tool_schemas`
  (`crates/kerness/src/toolschema.rs:100`)
  treats any non-Anthropic native dialect as OpenAI-shaped; a fourth shape must
  not fall into it.
- Changing argument decoding → keep the native and text-fence decoders
  distinct; the difference is documented at
  `crates/kerness/src/toolschema.rs:178` and tested in both modules.
- Safe extension points: none of the functions hold state, so a new consumer
  calls them directly. Do not add a dialect-aware branch above this module.
- Forbidden coupling: this module must not import `session` or `agent_runtime`;
  those call down.
- Compatibility: `ToolDialect` is provider configuration and its serialized
  name appears in the run checkpoint (`AgentTurn` stores the dialect); a
  rename affects the public enum and [sessionfile.md](sessionfile.md)'s schema.

Improvement candidates (proposals, not accepted work):

- A `dialect_from_py` case with an unknown value in `test_toolschema.py`;
  success check: `ValueError` with the offending text.

## Open Gaps / Roadmap

- Three dialects cover the four shipped providers. A `CustomProvider` against an
  endpoint with a fourth shape has to use the prompt fallback.
- Streaming tool calls are not parsed, because responses are not streamed
  ([provider.md](provider.md); M4 on the root roadmap).
