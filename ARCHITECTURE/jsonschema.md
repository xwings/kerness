---
eatmycode_version: "1.1.0"
---

# JSON Schema

## Goal

Two jobs on the same data. `ensure_strict` rewrites a tool's parameter schema
into the shape providers require for strict function calling — every property
required, no additional properties, recursively. `validate_arguments` checks a
model's actual arguments against that schema and returns a list of
human-readable problems, which is what the dispatcher feeds back to the model
when a call is malformed.

The module owns neither the schema's origin (a `ToolSpec` from
[toolkit.md](toolkit.md), or a `pydantic` model on the Python side) nor what is
done with a validation failure. It is not a conforming JSON Schema validator
and does not try to be.

## Status

`done`. `cargo test -p kerness jsonschema` passes 8 tests and
`bindings/python/tests/test_jsonschema.py` passes its one boundary case.

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/jsonschema.rs` | both functions and their helpers |
| `bindings/python/src/funcs.rs` | `validate_arguments` (`bindings/python/src/funcs.rs:208`) and `ensure_strict` (`:220`) as pyfunctions |
| `bindings/python/kerness/jsonschema.py` | re-export shim |

## Language and Conventions

Rust crate module with two `#[pyfunction]` wrappers and a shim. The root's
[Coding Style and Code Design](../ARCHITECTURE.md#coding-style-and-code-design)
rules apply; local facts:

- The module imports only `error` and `pyfmt`. Failure messages are rendered
  through `pyfmt::repr` (`crates/kerness/src/jsonschema.rs:11`), so an enum
  refusal quotes the choices the way Python would spell them — those strings
  go back to the model and are asserted byte for byte in
  `enum_failures_quote_the_choices_the_way_python_would` (`:334`).
- `ensure_strict` mutates in place and returns `Result<()>`; the Python
  wrapper copies the value across, rewrites, and returns the copy
  (`bindings/python/src/funcs.rs:220`), so a Python caller sees a
  pure function.
- `unreachable!` with a message is used where a `get`/`get_mut` pair on the
  same key cannot disagree (`crates/kerness/src/jsonschema.rs:58`, `:73`,
  `:83`); the crate-wide convention for an invariant the borrow checker
  forces into two statements.
- Unit tests are inline under `#[cfg(test)]` with one helper, `args`
  (`crates/kerness/src/jsonschema.rs:287`), and sentence-style names.

## Design and Invariants

### Two functions, two depths

`ensure_strict` (`crates/kerness/src/jsonschema.rs:18`) is recursive: `strict`
(`:25`) walks `$defs`/`definitions` (`:35`), `properties` (`:51`), `items`
(`:66`), `anyOf` (`:71`) and `allOf` (`:82`), carrying a breadcrumb `path` so
a refusal names where in the document it broke. Every object gains
`additionalProperties: false` unless it already says otherwise (`:45`), every
declared property is promoted to `required` (`:53`), a null `default` is
dropped (`:108`), a single-element `allOf` is inlined (`:90`), and a `$ref`
with siblings is inlined with the siblings winning and the merged object
re-run (`:115`). A non-object where an object is required is an
`Error::Session` naming the path (`:27`), not a best effort.

`validate_arguments` (`crates/kerness/src/jsonschema.rs:183`) is deliberately
shallow. It checks required and
unexpected keys, top-level property types, and `enum` membership; it
recognizes object and array values but does not descend into them. Recursive
traversal belongs to strict-schema rewriting; the second function catches the
mistakes models make rather than implementing JSON Schema.

### Invariants a change must preserve

- `validate_arguments` never raises. An invalid call is a normal event in a
  model conversation, and the messages go back to the model as text. Both
  callers rely on it — `crates/kerness/src/toolkit.rs:87` turns a non-empty
  list into a `ToolResult::error`, and `crates/kerness/src/session/run.rs:844`
  into a tool error — so a `Result` here would change what a failing call
  costs (a tool result versus a failed turn).
- The messages read as instructions, not validator output: `missing required
  argument 'x'`, `unexpected argument 'x'`, `argument 'x' must be integer, got
  string`. Enforced by `required_and_type_failures_read_as_instructions`
  (`crates/kerness/src/jsonschema.rs:308`).
- `additionalProperties: false` applies to an empty `properties` too: a tool
  that takes no arguments rejects every argument rather than ignoring them.
  Enforced by `a_closed_empty_object_rejects_every_argument`
  (`crates/kerness/src/jsonschema.rs:295`) and the one Python case (`bindings/python/tests/test_jsonschema.py:6`).
- A boolean is not a number (`a_boolean_is_not_a_number`,
  `crates/kerness/src/jsonschema.rs:325`), because `serde_json` would
  otherwise let `true` satisfy an integer field.
- `$ref` resolves against the document root only, through `$defs` and
  `definitions` (`resolve_ref`, `crates/kerness/src/jsonschema.rs:138`);
  there is no remote or cross-document resolution.

## Key Types and Entry Points

- `crates/kerness/src/jsonschema.rs:18` — `ensure_strict(schema: &mut Value)
  -> Result<()>` — rewrites in place; `Error::Session` for a schema it cannot
  make strict, with the offending path in the message.
- `crates/kerness/src/jsonschema.rs:183` — `validate_arguments(schema,
  arguments) -> Vec<String>` — every problem found, empty when valid; never
  raises, and a schema that is not an object validates everything.
- `crates/kerness/src/jsonschema.rs:138` — `resolve_ref(root, reference)` —
  `#/`-anchored lookup only.
- `bindings/python/src/funcs.rs:220` — `ensure_strict(json_schema)` — the
  Python signature: takes a dict, returns a new dict, raises `SessionError`.
- `bindings/python/src/funcs.rs:208` — `validate_arguments(schema, arguments)`
  — returns `list[str]`.

## Interactions

- [toolkit.md](toolkit.md)'s `ToolDispatcher::execute` calls
  `validate_arguments` before every handler (`crates/kerness/src/toolkit.rs:87`)
  and hands the joined messages back as a `ToolResult`; [run.md](run.md)'s
  `tool_step` does the same before a scoped invocation
  (`crates/kerness/src/session/run.rs:844`). Both are proved through
  `crates/kerness/tests/tools_e2e.rs`.
- [provider.md](provider.md)'s OpenAI backend calls `ensure_strict` over a
  caller's output schema when `strict_json_schema` is set
  (`crates/kerness/src/provider/openai.rs:88`); on the Python side that schema
  comes from a `pydantic` `TypeAdapter`
  (`bindings/python/kerness/provider.py:312`). The Python provider tests
  `test_strict_mode_is_what_makes_an_optional_field_required` and
  `test_the_strict_rewrite_reaches_nested_models_too`
  (`bindings/python/tests/test_provider.py:176`, `:202`) are where the rewrite
  is proved against a real `pydantic` document.
- [toolschema.md](toolschema.md) renders `ToolSpec.parameters` into each
  dialect's wire shape without rewriting it; `ensure_strict` is applied only
  where a provider's strict mode demands it.

## How to Test

```sh
cargo test -p kerness jsonschema                                       # pass = 8 passed
.venv/bin/python -m pytest bindings/python/tests/test_jsonschema.py -q # pass = 1 passed
```

- The Rust tests are where the coverage is:
  `strict_closes_objects_and_requires_every_property`
  (`crates/kerness/src/jsonschema.rs:343`),
  `a_ref_with_siblings_is_inlined_and_the_siblings_win` (`:374`),
  `a_single_element_all_of_is_inlined` (`:363`),
  `a_non_object_schema_is_refused` (`:388`), and two that pin the failure
  messages — `enum_failures_quote_the_choices_the_way_python_would` (`:334`)
  and `required_and_type_failures_read_as_instructions` (`:308`) — because
  those strings go back to the model and are the module's real output.
- `bindings/python/tests/test_jsonschema.py:6`
  `test_closed_empty_object_rejects_every_argument`: the one boundary case, a
  dict crossing and the list coming back.
- The rewrite against a generated `pydantic` schema is proved in
  `bindings/python/tests/test_provider.py:176` and `:202`, not here.
- Gap: `anyOf` recursion has no dedicated test; it is exercised only when a
  `pydantic` `Optional` field reaches
  `bindings/python/tests/test_provider.py:176`.

## Review and Refactor Guide

- **Changing a failure message** → the two message-pinning tests
  (`crates/kerness/src/jsonschema.rs:308`, `:334`) and any Python test asserting the joined string through a tool
  result (`bindings/python/tests/test_toolkit.py`, `test_session.py`
  `TestToolCalls`). The text is model-facing output.
- **Adding a composition keyword** (`oneOf`, `not`) → a new branch in `strict`
  (`crates/kerness/src/jsonschema.rs:25`) following the `anyOf` shape at `:71`, with the breadcrumb extended
  through `extend` (`:159`), plus a test beside `:363`.
- **Making validation deeper** → keep it infallible and keep the messages
  instruction-shaped; both callers join the list with `; ` and return it to
  the model.
- Safe extension points: `type_matches` (`crates/kerness/src/jsonschema.rs:239`)
  for a new `type` spelling;
  `resolve_ref` (`:138`) for another local container.
- Forbidden coupling: this module must not import `tooling` or `provider`; it
  takes a bare `Value` so both a tool schema and an output schema can go
  through it.
- Compatibility: the Python `ensure_strict` returns a new dict; callers that
  relied on mutation would silently see nothing.

Improvement candidates (proposals, not accepted work):

- A direct `anyOf` test in the Rust suite would let the strict rewrite be
  changed without a `pydantic` install to prove it. Success check: the test
  fails when the `anyOf` branch is removed.

## Open Gaps / Roadmap

- `$ref` resolves against the document only — `$defs` and `definitions`
  (`crates/kerness/src/jsonschema.rs:35`), which is what a `pydantic` model
  with a nested submodel emits. There is no remote or cross-document
  resolution, and none is planned.
- `validate_arguments` reports every problem it finds but does not attempt
  coercion — a string `"3"` for an integer field is reported, not converted.
- `oneOf` is not handled; `anyOf` and `allOf` are
  (`crates/kerness/src/jsonschema.rs:71`, `:82`). Nothing the framework
  generates emits `oneOf`, so this bites only a caller hand-writing a tool
  schema.
