//! Result validation independent of the legacy coercion adapter.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::harness::{ResultField, ResultType};
use crate::tooling::extract_fenced_json;

/// Strict results retain supplied values and report contract errors. Coercion
/// is available explicitly for compatibility with `Session::run`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultValidation {
    #[default]
    Strict,
    LegacyCoercion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultIssue {
    MissingResult,
    MalformedResult { message: String },
    MissingField { field: String },
    WrongType { field: String, expected: String },
    UnexpectedField { field: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultDiagnostics {
    pub valid: bool,
    pub issues: Vec<ResultIssue>,
}

pub(super) fn validate(value: &Value, fields: &[ResultField]) -> ResultDiagnostics {
    let mut issues = Vec::new();
    let Some(values) = value.as_object() else {
        return ResultDiagnostics {
            valid: false,
            issues: vec![ResultIssue::MalformedResult {
                message: "Result must be a JSON object.".into(),
            }],
        };
    };
    for field in fields {
        let Some(value) = values.get(&field.name) else {
            issues.push(ResultIssue::MissingField {
                field: field.name.clone(),
            });
            continue;
        };
        let correct = match field.result_type() {
            ResultType::Bool => value.is_boolean(),
            ResultType::Int => value.is_i64() || value.is_u64(),
            ResultType::Float => value.is_number(),
            ResultType::Str => value.is_string(),
            ResultType::List => value.is_array(),
            ResultType::Dict => value.is_object(),
        };
        if !correct {
            issues.push(ResultIssue::WrongType {
                field: field.name.clone(),
                expected: field.type_name.clone(),
            });
        }
    }
    if !fields.is_empty() {
        for name in values.keys() {
            if !fields.iter().any(|field| field.name == *name) {
                issues.push(ResultIssue::UnexpectedField {
                    field: name.clone(),
                });
            }
        }
    }
    ResultDiagnostics {
        valid: issues.is_empty(),
        issues,
    }
}

pub(super) fn parse(text: &str, fields: &[ResultField]) -> (Map<String, Value>, ResultDiagnostics) {
    if fields.is_empty() {
        return (
            Map::new(),
            ResultDiagnostics {
                valid: true,
                issues: Vec::new(),
            },
        );
    }
    let fenced = extract_fenced_json(text, &["json"]);
    let raw = if !fenced.is_empty() {
        fenced.as_str()
    } else if text.trim().starts_with('{') {
        text.trim()
    } else if let Some(start) = text.find('{') {
        text[start..].trim()
    } else {
        return (
            Map::new(),
            ResultDiagnostics {
                valid: false,
                issues: vec![ResultIssue::MissingResult],
            },
        );
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => {
            let diagnostics = validate(&value, fields);
            (value.as_object().cloned().unwrap_or_default(), diagnostics)
        }
        Err(error) => (
            Map::new(),
            ResultDiagnostics {
                valid: false,
                issues: vec![ResultIssue::MalformedResult {
                    message: error.to_string(),
                }],
            },
        ),
    }
}
