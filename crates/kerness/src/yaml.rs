//! YAML parsing with PyYAML's scalar semantics.
//!
//! Gameplans and skill files are Markdown with YAML frontmatter, and the
//! frontmatter is a *contract* — a gameplan that parsed one way under the
//! Python framework and another way here is a gameplan that silently changed
//! meaning.
//!
//! PyYAML implements YAML **1.1**, where `yes`, `no`, `on`, and `off` are
//! booleans, a leading zero means octal, and an exponent without a sign is not
//! a number at all. Every modern YAML library implements 1.2, where none of
//! that holds. `verdict_rethink: no` is the case that matters: 1.2 reads it as
//! the string `"no"`, which the harness parser then rejects as "must be a
//! boolean" — a working gameplan that stops loading.
//!
//! So the parser here reads *events*, not values. Only a plain (unquoted)
//! scalar is resolved; `"no"` in quotes stays a string, which is the
//! distinction a `Value`-level deserializer has already discarded by the time
//! anything can look at it.
//!
//! Two things PyYAML does are deliberately not reproduced: a plain scalar that
//! looks like a date stays a string rather than becoming a `datetime`, and
//! `.inf`/`.nan` stay strings because the JSON value model has no room for
//! them. Neither is expressible in a harness field.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Number, Value};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

/// The core tag handle, which is what `!!str` and friends expand to.
const CORE_TAG: &str = "tag:yaml.org,2002:";

/// Parse a single YAML document.
///
/// An empty document is `null`, as it is in PyYAML. A stream holding more than
/// one document is an error rather than a silent choice of which one to use.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser::new_from_str(text);
    let mut events = Vec::new();
    loop {
        let (event, _marker) = parser.next_token().map_err(|err| err.to_string())?;
        if event == Event::StreamEnd {
            break;
        }
        events.push(event);
    }

    let documents = events
        .iter()
        .filter(|event| **event == Event::DocumentStart)
        .count();
    if documents > 1 {
        return Err("expected a single document in the stream".to_string());
    }

    let mut cursor = 0usize;
    let mut anchors = HashMap::new();
    while cursor < events.len()
        && matches!(events[cursor], Event::StreamStart | Event::DocumentStart)
    {
        cursor += 1;
    }
    if cursor >= events.len() || events[cursor] == Event::DocumentEnd {
        return Ok(Value::Null);
    }
    build(&events, &mut cursor, &mut anchors)
}

/// Build one node, advancing *cursor* past everything it consumed.
fn build(
    events: &[Event],
    cursor: &mut usize,
    anchors: &mut HashMap<usize, Value>,
) -> Result<Value, String> {
    let Some(event) = events.get(*cursor) else {
        return Err("unexpected end of YAML stream".to_string());
    };
    *cursor += 1;

    match event {
        Event::Scalar(text, style, anchor, tag) => {
            let value = resolve_scalar(text, *style, tag.as_ref())?;
            remember(anchors, *anchor, value)
        }
        Event::SequenceStart(anchor, _tag) => {
            let mut items = Vec::new();
            while events.get(*cursor) != Some(&Event::SequenceEnd) {
                items.push(build(events, cursor, anchors)?);
            }
            *cursor += 1;
            remember(anchors, *anchor, Value::Array(items))
        }
        Event::MappingStart(anchor, _tag) => {
            let mapping = build_mapping(events, cursor, anchors)?;
            remember(anchors, *anchor, Value::Object(mapping))
        }
        Event::Alias(anchor) => anchors
            .get(anchor)
            .cloned()
            .ok_or_else(|| format!("found undefined alias for anchor {anchor}")),
        other => Err(format!("unexpected YAML event: {other:?}")),
    }
}

fn build_mapping(
    events: &[Event],
    cursor: &mut usize,
    anchors: &mut HashMap<usize, Value>,
) -> Result<Map<String, Value>, String> {
    let mut mapping = Map::new();
    let mut merged: Vec<Map<String, Value>> = Vec::new();

    while events.get(*cursor) != Some(&Event::MappingEnd) {
        let merge_key = matches!(
            events.get(*cursor),
            Some(Event::Scalar(text, TScalarStyle::Plain, _, None)) if text == "<<"
        );
        let key = build(events, cursor, anchors)?;
        let value = build(events, cursor, anchors)?;
        if merge_key {
            collect_merge(value, &mut merged)?;
            continue;
        }
        mapping.insert(key_text(&key), value);
    }
    *cursor += 1;

    // An explicit key always wins over a merged one, and an earlier merge
    // source wins over a later one — the order PyYAML's `flatten_mapping`
    // produces.
    for source in merged {
        for (key, value) in source {
            mapping.entry(key).or_insert(value);
        }
    }
    Ok(mapping)
}

/// A `<<` value is one mapping or a sequence of them.
fn collect_merge(value: Value, merged: &mut Vec<Map<String, Value>>) -> Result<(), String> {
    match value {
        Value::Object(mapping) => merged.push(mapping),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(mapping) => merged.push(mapping),
                    other => {
                        return Err(format!(
                            "expected a mapping for merging, but found {}",
                            type_word(&other)
                        ))
                    }
                }
            }
        }
        other => {
            return Err(format!(
                "expected a mapping or list of mappings for merging, but found {}",
                type_word(&other)
            ))
        }
    }
    Ok(())
}

fn type_word(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "sequence",
        Value::Object(_) => "mapping",
    }
}

/// Record *value* under its anchor, if it has one, and return it.
fn remember(
    anchors: &mut HashMap<usize, Value>,
    anchor: usize,
    value: Value,
) -> Result<Value, String> {
    // Anchor ids start at 1; 0 means the node was not anchored.
    if anchor != 0 {
        anchors.insert(anchor, value.clone());
    }
    Ok(value)
}

/// A mapping key as Python would spell it.
///
/// JSON objects are keyed by strings and YAML mappings are not, so a numeric
/// or boolean key is rendered the way `str()` renders it — which is what every
/// caller of a parsed key does anyway.
fn key_text(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        other => crate::pyfmt::str(other),
    }
}

/// Turn one scalar into a value.
///
/// A quoted, literal, or folded scalar is always a string. A plain one is
/// resolved. An explicit `!!tag` overrides both.
fn resolve_scalar(
    text: &str,
    style: TScalarStyle,
    tag: Option<&Tag>,
) -> Result<Value, String> {
    if let Some(tag) = tag {
        if tag.handle != CORE_TAG {
            return Err(format!(
                "could not determine a constructor for the tag '{}{}'",
                tag.handle, tag.suffix
            ));
        }
        return match tag.suffix.as_str() {
            "str" => Ok(Value::String(text.to_string())),
            "bool" => parse_bool(text)
                .ok_or_else(|| format!("could not determine a constructor for bool '{text}'")),
            "int" => parse_int(text)
                .ok_or_else(|| format!("could not determine a constructor for int '{text}'")),
            "float" => parse_float(text)
                .ok_or_else(|| format!("could not determine a constructor for float '{text}'")),
            "null" => Ok(Value::Null),
            other => Err(format!(
                "could not determine a constructor for the tag '{CORE_TAG}{other}'"
            )),
        };
    }
    if style != TScalarStyle::Plain {
        return Ok(Value::String(text.to_string()));
    }
    Ok(resolve_plain(text))
}

/// PyYAML's implicit resolvers, in the order it registers them.
fn resolve_plain(text: &str) -> Value {
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return Value::Null;
    }
    if let Some(value) = parse_bool(text) {
        return value;
    }
    if let Some(value) = parse_int(text) {
        return value;
    }
    if let Some(value) = parse_float(text) {
        return value;
    }
    Value::String(text.to_string())
}

static BOOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$")
        .expect("static pattern")
});

static INT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:[-+]?0b[0-1_]+",
        r"|[-+]?0[0-7_]+",
        r"|[-+]?(?:0|[1-9][0-9_]*)",
        r"|[-+]?0x[0-9a-fA-F_]+",
        r"|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$",
    ))
    .expect("static pattern")
});

static FLOAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    // The exponent's sign is mandatory, which is YAML 1.1: `1.0e5` is a
    // string, `1.0e+5` is a float.
    Regex::new(concat!(
        r"^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?",
        r"|\.[0-9_]+(?:[eE][-+][0-9]+)?",
        r"|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*",
        r"|[-+]?\.(?:inf|Inf|INF)",
        r"|\.(?:nan|NaN|NAN))$",
    ))
    .expect("static pattern")
});

fn parse_bool(text: &str) -> Option<Value> {
    if !BOOL_RE.is_match(text) {
        return None;
    }
    Some(Value::Bool(matches!(
        text.to_ascii_lowercase().as_str(),
        "yes" | "true" | "on"
    )))
}

fn parse_int(text: &str) -> Option<Value> {
    if !INT_RE.is_match(text) {
        return None;
    }
    let cleaned = text.replace('_', "");
    let (negative, digits) = split_sign(&cleaned);
    let magnitude = if let Some(rest) = digits.strip_prefix("0b") {
        i64::from_str_radix(rest, 2).ok()?
    } else if let Some(rest) = digits.strip_prefix("0x") {
        i64::from_str_radix(rest, 16).ok()?
    } else if digits.contains(':') {
        sexagesimal(digits)? as i64
    } else if digits.len() > 1 && digits.starts_with('0') {
        i64::from_str_radix(&digits[1..], 8).ok()?
    } else {
        digits.parse::<i64>().ok()?
    };
    Some(Value::Number(Number::from(if negative {
        -magnitude
    } else {
        magnitude
    })))
}

fn parse_float(text: &str) -> Option<Value> {
    if !FLOAT_RE.is_match(text) {
        return None;
    }
    let cleaned = text.replace('_', "");
    let (negative, digits) = split_sign(&cleaned);
    let magnitude = if digits.contains(':') {
        sexagesimal(digits)?
    } else if digits.eq_ignore_ascii_case(".inf") {
        f64::INFINITY
    } else if digits.eq_ignore_ascii_case(".nan") {
        f64::NAN
    } else {
        digits.parse::<f64>().ok()?
    };
    let signed = if negative { -magnitude } else { magnitude };
    // JSON has no infinity and no NaN. Neither can reach a harness field, so
    // the scalar keeps the text it was written with rather than becoming null.
    Number::from_f64(signed)
        .map(Value::Number)
        .or_else(|| Some(Value::String(text.to_string())))
}

fn split_sign(text: &str) -> (bool, &str) {
    match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    }
}

/// Base-60, as in `1:30` for ninety.
fn sexagesimal(digits: &str) -> Option<f64> {
    let mut total = 0.0f64;
    for part in digits.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(text: &str) -> Value {
        parse(text).expect("valid YAML")
    }

    #[test]
    fn an_empty_document_is_null() {
        assert_eq!(parsed(""), Value::Null);
        assert_eq!(parsed("   \n"), Value::Null);
    }

    #[test]
    fn yaml_one_point_one_booleans_are_still_booleans() {
        assert_eq!(
            parsed("verdict_rethink: no\nrethink: yes\nquiet: off\nloud: on"),
            json!({"verdict_rethink": false, "rethink": true, "quiet": false, "loud": true})
        );
    }

    #[test]
    fn quoting_is_what_keeps_a_word_a_word() {
        assert_eq!(
            parsed("plain: no\nquoted: \"no\"\nsingle: 'no'"),
            json!({"plain": false, "quoted": "no", "single": "no"})
        );
    }

    #[test]
    fn a_single_letter_is_not_a_boolean() {
        // PyYAML lists `y` as a first character but its pattern has no `y`
        // alternative, so `y` is a plain string there and must be here.
        assert_eq!(parsed("answer: y"), json!({"answer": "y"}));
    }

    #[test]
    fn numbers_follow_the_one_point_one_rules() {
        assert_eq!(
            parsed("dec: 42\noct: 0755\nhex: 0x1f\nbin: 0b1010\ngrouped: 1_000"),
            json!({"dec": 42, "oct": 493, "hex": 31, "bin": 10, "grouped": 1000})
        );
        assert_eq!(parsed("mins: 1:30"), json!({"mins": 90}));
        assert_eq!(parsed("neg: -0x10"), json!({"neg": -16}));
    }

    #[test]
    fn an_unsigned_exponent_is_not_a_number() {
        assert_eq!(
            parsed("loose: 1.0e5\nstrict: 1.0e+5"),
            json!({"loose": "1.0e5", "strict": 100000.0})
        );
    }

    #[test]
    fn nulls_are_spelled_four_ways_and_an_absent_value_is_one() {
        assert_eq!(
            parsed("a: ~\nb: null\nc: NULL\nd:"),
            json!({"a": null, "b": null, "c": null, "d": null})
        );
    }

    #[test]
    fn nested_structures_come_through_whole() {
        let document = parsed(concat!(
            "loop:\n",
            "  max_rounds: 3\n",
            "  phases:\n",
            "    - name: think\n",
            "      rounds: 1\n",
            "    - name: rethink\n",
            "      rethink: true\n",
        ));
        assert_eq!(document["loop"]["max_rounds"], json!(3));
        assert_eq!(document["loop"]["phases"][1]["name"], json!("rethink"));
        assert_eq!(document["loop"]["phases"][1]["rethink"], json!(true));
    }

    #[test]
    fn anchors_are_reused_and_merges_lose_to_explicit_keys() {
        let document = parsed(concat!(
            "base: &base\n",
            "  rounds: 1\n",
            "  rethink: false\n",
            "phase:\n",
            "  <<: *base\n",
            "  rethink: true\n",
        ));
        assert_eq!(document["phase"], json!({"rethink": true, "rounds": 1}));
    }

    #[test]
    fn an_explicit_tag_overrides_the_resolver() {
        assert_eq!(parsed("value: !!str 42"), json!({"value": "42"}));
    }

    #[test]
    fn a_stream_with_two_documents_is_refused() {
        let error = parse("a: 1\n---\nb: 2\n").expect_err("two documents");
        assert!(error.contains("single document"), "{error}");
    }

    #[test]
    fn malformed_yaml_reports_where_it_broke() {
        let error = parse("a:\n  - 1\n b: 2\n").expect_err("bad indentation");
        assert!(error.contains("line"), "{error}");
    }
}
