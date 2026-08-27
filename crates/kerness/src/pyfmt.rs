//! Rendering JSON values the way Python renders them.
//!
//! Framework text that a model reads — the tool prompt, validation errors,
//! coerced result fields — was written against `json.dumps` and `repr`.
//! `serde_json`'s defaults differ in separators and in non-ASCII escaping, and
//! those differences are visible in every prompt, so they are corrected here
//! once rather than worked around at each call site.

use std::io;

use serde_json::ser::{CharEscape, Formatter, Serializer};
use serde_json::Value;

/// `json.dumps(value, ensure_ascii=True)` — compact-with-spaces, ASCII only.
pub fn json_dumps(value: &Value) -> String {
    let mut out = Vec::with_capacity(64);
    let mut serializer = Serializer::with_formatter(&mut out, PythonFormatter);
    serde::Serialize::serialize(value, &mut serializer).expect("writing to a Vec cannot fail");
    String::from_utf8(out).expect("the formatter only emits ASCII and valid UTF-8")
}

/// `json.dumps(value, indent=2, ensure_ascii=False)` — the session-file shape.
pub fn json_dumps_indent2(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a Value always serializes")
}

/// Python's `repr`.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() => {
                let rendered = float.to_string();
                if rendered.contains(['.', 'e', 'E', 'i', 'N']) {
                    rendered
                } else {
                    format!("{rendered}.0")
                }
            }
            _ => number.to_string(),
        },
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let rendered: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", rendered.join(", "))
        }
        Value::Object(entries) => {
            let rendered: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{}: {}", repr_str(key), repr(value)))
                .collect();
            format!("{{{}}}", rendered.join(", "))
        }
    }
}

/// Python's `str`, which differs from [`repr`] only for strings.
pub fn str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => repr(other),
    }
}

/// Python's truthiness: empty containers, zero, `False`, and `None` are false.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(entries) => !entries.is_empty(),
    }
}

/// Python's `repr` for a string: single quotes unless that would mean escaping
/// an apostrophe the double-quoted form would not.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `json.dumps` separators (`", "` and `": "`) plus `ensure_ascii` escaping.
struct PythonFormatter;

impl PythonFormatter {
    fn escape_non_ascii<W>(writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let mut plain_from = 0usize;
        for (offset, character) in fragment.char_indices() {
            if character.is_ascii() {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[plain_from..offset])?;
            let mut buffer = [0u16; 2];
            for unit in character.encode_utf16(&mut buffer) {
                write!(writer, "\\u{unit:04x}")?;
            }
            plain_from = offset + character.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[plain_from..])
    }
}

impl Formatter for PythonFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        Self::escape_non_ascii(writer, fragment)
    }

    fn write_char_escape<W>(&mut self, writer: &mut W, escape: CharEscape) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        // `serde_json` escapes `/` never and control characters as `\u00XX`,
        // which is what Python does too; only the named escapes need matching.
        match escape {
            CharEscape::Quote => writer.write_all(b"\\\""),
            CharEscape::ReverseSolidus => writer.write_all(b"\\\\"),
            CharEscape::Solidus => writer.write_all(b"/"),
            CharEscape::Backspace => writer.write_all(b"\\b"),
            CharEscape::FormFeed => writer.write_all(b"\\f"),
            CharEscape::LineFeed => writer.write_all(b"\\n"),
            CharEscape::CarriageReturn => writer.write_all(b"\\r"),
            CharEscape::Tab => writer.write_all(b"\\t"),
            CharEscape::AsciiControl(byte) => write!(writer, "\\u{byte:04x}"),
        }
    }

    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(b": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_uses_python_separators() {
        assert_eq!(
            json_dumps(&json!({"type": "object", "properties": {}})),
            r#"{"type": "object", "properties": {}}"#
        );
        assert_eq!(json_dumps(&json!([1, 2, 3])), "[1, 2, 3]");
        assert_eq!(json_dumps(&json!({})), "{}");
    }

    #[test]
    fn dumps_escapes_non_ascii() {
        // ensure_ascii=True: every non-ASCII character leaves as a \uXXXX
        // escape, surrogate pair included.
        let escape = char::from(92);
        assert_eq!(
            json_dumps(&json!("café")),
            format!("\"caf{escape}u00e9\"")
        );
        assert_eq!(
            json_dumps(&json!("𝄞")),
            format!("\"{escape}ud834{escape}udd1e\"")
        );
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr(&json!(null)), "None");
        assert_eq!(repr(&json!(true)), "True");
        assert_eq!(repr(&json!(1.0)), "1.0");
        assert_eq!(repr(&json!(1)), "1");
        assert_eq!(repr(&json!("it's")), "\"it's\"");
        assert_eq!(repr(&json!(["a", "b"])), "['a', 'b']");
        assert_eq!(repr(&json!({"a": 1})), "{'a': 1}");
    }

    #[test]
    fn str_unwraps_only_strings() {
        assert_eq!(str(&json!("plain")), "plain");
        assert_eq!(str(&json!([1, 2])), "[1, 2]");
    }

    #[test]
    fn truthiness_follows_python() {
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!("")));
        assert!(!truthy(&json!([])));
        assert!(truthy(&json!("0")));
        assert!(truthy(&json!([0])));
    }
}
