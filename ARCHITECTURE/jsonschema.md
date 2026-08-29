# JSON Schema

## Goal

Two jobs on the same data. `ensure_strict` rewrites a tool's parameter schema
into the shape providers require for strict function calling — every property
required, no additional properties, recursively. `validate_arguments` checks a
model's actual arguments against that schema and returns a list of human-readable
problems, which is what the dispatcher feeds back to the model when a call is
malformed.

## Status

`done`

## Code Structure

| File | Role |
| ---- | ---- |
| `crates/kerness/src/jsonschema.rs` | both functions |
| `bindings/python/src/funcs.rs:205,217` | the two pyfunctions |
| `bindings/python/kerness/jsonschema.py` | re-export shim |

## Key Types and Entry Points

- `crates/kerness/src/jsonschema.rs:21` — `ensure_strict(schema)` — mutates in
  place and returns `Result`; a schema it cannot make strict is an error, not a
  best effort. Recurses through `properties`, `items`, and the composition
  keywords.
- `crates/kerness/src/jsonschema.rs:184` — `validate_arguments(schema, arguments)` —
  returns `Vec<String>`, empty when valid. It never raises: an invalid call is a
  normal event in a model conversation, and the messages go back to the model.

This is a targeted validator, not a general JSON Schema implementation. It covers
the keywords a tool parameter schema uses — types, `required`, `enum`, nested
objects and arrays — because that is the whole domain: schemas the framework
either generated or received alongside a tool it is about to call.

## Interactions

- Applied to every tool's schema by [toolschema.md](toolschema.md) when building
  native tool definitions.
- Used by [provider.md](provider.md) for structured output: `ensure_strict` runs
  over the schema a `pydantic` model produced before it is sent.
- Its messages are returned to the model by [toolkit.md](toolkit.md)'s dispatcher.

## How to Test

```sh
cargo test -p kerness jsonschema                                       # pass = 0 failed
.venv/bin/python -m pytest bindings/python/tests/test_jsonschema.py -q # pass = 0 failed
```

- The Rust tests are where the coverage is: `strict_closes_objects_and_requires_every_property`,
  `a_ref_with_siblings_is_inlined_and_the_siblings_win`,
  `a_single_element_all_of_is_inlined`, `a_non_object_schema_is_refused`, and two
  that pin the failure messages —
  `enum_failures_quote_the_choices_the_way_python_would` and
  `required_and_type_failures_read_as_instructions`, because those strings go back
  to the model and are the module's real output.
- `bindings/python/tests/test_jsonschema.py` holds one case, `test_closed_empty_object_rejects_every_argument`:
  a tool that takes no arguments must reject every argument rather than ignore
  them.

## Open Gaps / Roadmap

- `$ref` resolves against the document only — `$defs` and `definitions`
  (`jsonschema.rs:36`), which is what a `pydantic` model with a nested submodel
  emits. There is no remote or cross-document resolution, and none is planned.
- `validate_arguments` reports every problem it finds but does not attempt
  coercion — a string `"3"` for an integer field is reported, not converted.
- `oneOf` is not handled; `anyOf` and `allOf` are (`jsonschema.rs:72`, `:83`).
  Nothing the framework generates emits `oneOf`, so this bites only a caller
  hand-writing a tool schema.
