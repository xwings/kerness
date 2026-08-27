//! JSON Schema strictness and argument validation.
//!
//! Two jobs that look related and are not. [`ensure_strict`] rewrites a schema
//! so a provider's strict mode will accept it. [`validate_arguments`] checks
//! what a model actually sent. The second is deliberately shallow: it catches
//! the mistakes models make, and is not a conforming JSON Schema validator.

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::pyfmt::{repr as py_repr, repr_str as py_repr_str};

/// Rewrite *schema* in place to satisfy strict-mode requirements.
///
/// Sets `additionalProperties: false` on every object, promotes every declared
/// property to `required`, inlines single-element `allOf` and `$ref` siblings,
/// and drops null defaults.
///
/// `path` is a breadcrumb that only ever appears in error messages; `root` is
/// the document `$ref`s resolve against and defaults to *schema* itself.
pub fn ensure_strict(schema: &mut Value) -> Result<()> {
    let root = schema.clone();
    strict(schema, &[], &root)
}

fn strict(schema: &mut Value, path: &[String], root: &Value) -> Result<()> {
    if !schema.is_object() {
        return Err(Error::session(format!(
            "Expected {} to be a dictionary; path={}",
            py_repr(schema),
            render_path(path)
        )));
    }
    let object = schema.as_object_mut().expect("checked a statement ago");

    for container in ["$defs", "definitions"] {
        if let Some(Value::Object(entries)) = object.get_mut(container) {
            let names: Vec<String> = entries.keys().cloned().collect();
            for name in names {
                let child = entries.get_mut(&name).expect("key just enumerated");
                strict(child, &extend(path, &[container, &name]), root)?;
            }
        }
    }

    if object.get("type") == Some(&Value::String("object".into()))
        && !object.contains_key("additionalProperties")
    {
        object.insert("additionalProperties".into(), Value::Bool(false));
    }

    if let Some(Value::Object(properties)) = object.get("properties") {
        let names: Vec<String> = properties.keys().cloned().collect();
        object.insert(
            "required".into(),
            Value::Array(names.iter().cloned().map(Value::String).collect()),
        );
        let Some(Value::Object(properties)) = object.get_mut("properties") else {
            unreachable!("properties was an object a statement ago");
        };
        for name in names {
            let child = properties.get_mut(&name).expect("key just enumerated");
            strict(child, &extend(path, &["properties", &name]), root)?;
        }
    }

    if matches!(object.get("items"), Some(Value::Object(_))) {
        let items = object.get_mut("items").expect("just matched");
        strict(items, &extend(path, &["items"]), root)?;
    }

    if matches!(object.get("anyOf"), Some(Value::Array(_))) {
        let Some(Value::Array(variants)) = object.get_mut("anyOf") else {
            unreachable!("anyOf was an array a statement ago");
        };
        let mut variants = std::mem::take(variants);
        for (index, variant) in variants.iter_mut().enumerate() {
            strict(variant, &extend(path, &["anyOf", &index.to_string()]), root)?;
        }
        object.insert("anyOf".into(), Value::Array(variants));
    }

    if matches!(object.get("allOf"), Some(Value::Array(_))) {
        let Some(Value::Array(entries)) = object.get_mut("allOf") else {
            unreachable!("allOf was an array a statement ago");
        };
        let mut entries = std::mem::take(entries);
        for (index, entry) in entries.iter_mut().enumerate() {
            strict(entry, &extend(path, &["allOf", &index.to_string()]), root)?;
        }
        if entries.len() == 1 {
            // A single-element allOf says nothing a merge does not say, and
            // strict mode rejects the wrapper.
            let Some(Value::Object(only)) = entries.into_iter().next() else {
                return Err(Error::session(format!(
                    "Expected allOf entry to be a dictionary; path={}",
                    render_path(&extend(path, &["allOf", "0"]))
                )));
            };
            for (key, value) in only {
                object.insert(key, value);
            }
            object.remove("allOf");
        } else {
            object.insert("allOf".into(), Value::Array(entries));
        }
    }

    if object.get("default") == Some(&Value::Null) {
        object.remove("default");
    }

    // A `$ref` with siblings is not legal strict mode: the siblings are
    // silently ignored by some validators and rejected by others. Inline the
    // target and let the siblings win, then re-run over the merged result.
    let inline = match object.get("$ref") {
        Some(Value::String(reference)) if !reference.is_empty() && object.len() > 1 => {
            Some(reference.clone())
        }
        _ => None,
    };
    if let Some(reference) = inline {
        let resolved = resolve_ref(root, &reference)?;
        let Value::Object(resolved) = resolved else {
            return Err(Error::session(format!(
                "Expected `$ref: {reference}` to resolved to a dictionary but got {resolved}"
            )));
        };
        for (key, value) in resolved {
            object.entry(key).or_insert(value);
        }
        object.remove("$ref");
        return strict(schema, path, root);
    }

    Ok(())
}

fn resolve_ref(root: &Value, reference: &str) -> Result<Value> {
    let Some(rest) = reference.strip_prefix("#/") else {
        return Err(Error::session(format!(
            "Unexpected $ref format '{reference}'; Does not start with #/"
        )));
    };
    let mut resolved = root;
    for key in rest.split('/') {
        let next = resolved.get(key).ok_or_else(|| {
            Error::session(format!("unresolvable $ref {reference}: no key '{key}'"))
        })?;
        if !next.is_object() {
            return Err(Error::session(format!(
                "encountered non-dictionary entry while resolving {reference} - {resolved}"
            )));
        }
        resolved = next;
    }
    Ok(resolved.clone())
}

fn extend(path: &[String], parts: &[&str]) -> Vec<String> {
    let mut out = path.to_vec();
    out.extend(parts.iter().map(|p| (*p).to_string()));
    out
}

/// The breadcrumb as a tuple literal, which is how the message carrying it
/// reads.
fn render_path(path: &[String]) -> String {
    let parts: Vec<String> = path.iter().map(|p| py_repr_str(p)).collect();
    match parts.len() {
        0 => "()".into(),
        1 => format!("({},)", parts[0]),
        _ => format!("({})", parts.join(", ")),
    }
}

/// Validate tool-call *arguments* against a `parameters` schema.
///
/// Checks required keys, unexpected keys when `additionalProperties` is
/// `false`, top-level property types, and `enum` membership. Returns
/// human-readable messages, empty when valid — the dispatcher hands these
/// straight back to the model, so they read as instructions rather than as
/// validator output.
pub fn validate_arguments(schema: &Value, arguments: &Map<String, Value>) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(schema) = schema.as_object() else {
        return errors;
    };

    let empty = Map::new();
    let properties = match schema.get("properties") {
        Some(Value::Object(map)) => map,
        _ => &empty,
    };

    if let Some(Value::Array(required)) = schema.get("required") {
        for key in required {
            if let Value::String(key) = key {
                if !arguments.contains_key(key) {
                    errors.push(format!("missing required argument '{key}'"));
                }
            }
        }
    }

    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for key in arguments.keys() {
            if !properties.contains_key(key) {
                errors.push(format!("unexpected argument '{key}'"));
            }
        }
    }

    for (key, value) in arguments {
        let Some(Value::Object(property)) = properties.get(key) else {
            continue;
        };
        if let Some(Value::String(expected)) = property.get("type") {
            if !type_matches(value, expected) {
                errors.push(format!(
                    "argument '{key}' must be {expected}, got {}",
                    type_name(value)
                ));
            }
        }
        if let Some(Value::Array(choices)) = property.get("enum") {
            if !choices.is_empty() && !choices.iter().any(|choice| equal(choice, value)) {
                errors.push(format!(
                    "argument '{key}' must be one of {}, got {}",
                    py_repr(&Value::Array(choices.clone())),
                    py_repr(value)
                ));
            }
        }
    }

    errors
}

fn type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        // JSON has no boolean-as-number, whatever the host language thinks.
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // Unknown type keyword — do not reject.
        _ => true,
    }
}

/// The type name the validation message promises.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Equality over JSON values, as `enum` membership needs it: `1` and `1.0`
/// are the same number, and `true` matches `1`.
fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        },
        (Value::Bool(a), Value::Number(b)) | (Value::Number(b), Value::Bool(a)) => {
            b.as_f64() == Some(if *a { 1.0 } else { 0.0 })
        }
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: Value) -> Map<String, Value> {
        value
            .as_object()
            .cloned()
            .expect("test arguments are objects")
    }

    #[test]
    fn a_closed_empty_object_rejects_every_argument() {
        let schema = json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        });
        assert_eq!(
            validate_arguments(&schema, &args(json!({"surprise": 1}))),
            vec!["unexpected argument 'surprise'".to_string()]
        );
    }

    #[test]
    fn required_and_type_failures_read_as_instructions() {
        let schema = json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        });
        assert_eq!(
            validate_arguments(&schema, &args(json!({}))),
            vec!["missing required argument 'command'".to_string()]
        );
        assert_eq!(
            validate_arguments(&schema, &args(json!({"command": 7}))),
            vec!["argument 'command' must be string, got int".to_string()]
        );
    }

    #[test]
    fn a_boolean_is_not_a_number() {
        let schema = json!({"properties": {"n": {"type": "integer"}}});
        assert_eq!(
            validate_arguments(&schema, &args(json!({"n": true}))),
            vec!["argument 'n' must be integer, got bool".to_string()]
        );
    }

    #[test]
    fn enum_failures_quote_the_choices_the_way_python_would() {
        let schema = json!({"properties": {"mode": {"enum": ["fast", "slow"]}}});
        assert_eq!(
            validate_arguments(&schema, &args(json!({"mode": "medium"}))),
            vec!["argument 'mode' must be one of ['fast', 'slow'], got 'medium'".to_string()]
        );
    }

    #[test]
    fn strict_closes_objects_and_requires_every_property() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "string", "default": null},
                "b": {"type": "object", "properties": {"c": {"type": "integer"}}},
            },
        });
        ensure_strict(&mut schema).expect("valid schema");
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["a", "b"]));
        assert!(schema["properties"]["a"].get("default").is_none());
        assert_eq!(
            schema["properties"]["b"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(schema["properties"]["b"]["required"], json!(["c"]));
    }

    #[test]
    fn a_single_element_all_of_is_inlined() {
        let mut schema = json!({
            "allOf": [{"type": "object", "properties": {"x": {"type": "string"}}}],
        });
        ensure_strict(&mut schema).expect("valid schema");
        assert!(schema.get("allOf").is_none());
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["x"]));
    }

    #[test]
    fn a_ref_with_siblings_is_inlined_and_the_siblings_win() {
        let mut schema = json!({
            "$defs": {"Inner": {"type": "object", "properties": {"x": {"type": "string"}}}},
            "$ref": "#/$defs/Inner",
            "description": "mine",
        });
        ensure_strict(&mut schema).expect("valid schema");
        assert!(schema.get("$ref").is_none());
        assert_eq!(schema["description"], json!("mine"));
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn a_non_object_schema_is_refused() {
        let mut schema = json!([1, 2, 3]);
        assert!(ensure_strict(&mut schema).is_err());
    }
}
