//! Python-compatible JSON rendering.
//!
//! agenthicc prints most machine-readable output with `json.dumps(obj,
//! sort_keys=True)` (compact) or `json.dumps(obj, indent=2, sort_keys=True)`
//! (pretty). Python's compact form is *not* serde_json's: it separates items
//! with `", "` and keys with `": "`, and it escapes every non-ASCII character
//! (`ensure_ascii=True`). A consumer that diffs output, or hashes it, sees the
//! difference, so commands that port an agenthicc `json.dumps` render through
//! here.
//!
//! Keys are always sorted (by code point, which is the order of Rust's `str`
//! comparison). Floats follow Python's `repr`: shortest round-trip digits, and
//! an exponent written with an explicit sign and at least two digits
//! (`1e-07`, `1e+16`).

use std::fmt::Write as _;

use serde_json::Value;

/// `json.dumps(value, sort_keys=True)`.
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0, (", ", ": "));
    out
}

/// `json.dumps(value, indent=2, sort_keys=True)`.
pub fn dumps_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some(2), 0, (",", ": "));
    out
}

/// `json.dumps(dict)` *without* `sort_keys`: keys in the given order.
///
/// A few agenthicc error documents are printed from a dict literal without
/// `sort_keys=True`, so their keys appear in insertion order
/// (`{"session_id": …, "status": "error", "error": …}`). Nested values are
/// still rendered with sorted keys, which is all those documents contain.
pub fn dumps_ordered(pairs: &[(&str, Value)]) -> String {
    let mut out = String::from("{");
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_string(&mut out, key);
        out.push_str(": ");
        write_value(&mut out, value, None, 0, (", ", ": "));
    }
    out.push('}');
    out
}

/// `json.dumps(value, sort_keys=True, separators=(item, key))`.
pub fn dumps_with_separators(value: &Value, item: &str, key: &str) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0, (item, key));
    out
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(width) = indent {
        out.push('\n');
        for _ in 0..width * level {
            out.push(' ');
        }
    }
}

fn write_value(
    out: &mut String,
    value: &Value,
    indent: Option<usize>,
    level: usize,
    separators: (&str, &str),
) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(out, number),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(separators.0);
                }
                newline(out, indent, level + 1);
                write_value(out, item, indent, level + 1, separators);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(separators.0);
                }
                newline(out, indent, level + 1);
                write_string(out, key);
                out.push_str(separators.1);
                write_value(out, &map[key], indent, level + 1, separators);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, number: &serde_json::Number) {
    if number.is_f64() {
        let float = number.as_f64().unwrap_or(0.0);
        out.push_str(&python_float(float));
    } else {
        let _ = write!(out, "{number}");
    }
}

/// Python's `repr(float)` for a finite float.
fn python_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // `{:?}` is Rust's shortest round-trip form; reshape its exponent and
    // decide fixed/scientific the way Python's `float_repr_style` does
    // (scientific when the decimal exponent is < -4 or >= 16).
    let debug = format!("{value:e}");
    let (mantissa, exponent) = debug.split_once('e').unwrap_or((&debug, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..16).contains(&exponent) {
        let mut fixed = format!("{value:?}");
        if !fixed.contains('.') && !fixed.contains('e') && !fixed.contains("inf") {
            fixed.push_str(".0");
        }
        if fixed.contains('e') {
            // `{:?}` chose scientific for a value Python prints fixed.
            fixed = format!("{value}");
            if !fixed.contains('.') {
                fixed.push_str(".0");
            }
        }
        return fixed;
    }
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Python's `ESCAPE_ASCII` is `[^\ -~]`: everything outside
            // space..tilde, which includes DEL (0x7f).
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_uses_pythons_default_separators_and_sorted_keys() {
        let value = json!({"b": [1, 2], "a": {"y": null, "x": true}});
        assert_eq!(
            dumps(&value),
            r#"{"a": {"x": true, "y": null}, "b": [1, 2]}"#
        );
    }

    #[test]
    fn pretty_matches_indent_two() {
        let value = json!({"b": [], "a": {"x": 1}, "c": {}});
        assert_eq!(
            dumps_pretty(&value),
            "{\n  \"a\": {\n    \"x\": 1\n  },\n  \"b\": [],\n  \"c\": {}\n}"
        );
    }

    #[test]
    fn non_ascii_is_escaped_like_ensure_ascii() {
        assert_eq!(dumps(&json!("é")), r#""\u00e9""#);
        assert_eq!(dumps(&json!("😀")), r#""\ud83d\ude00""#);
        assert_eq!(dumps(&json!("a\"b\\c\nd")), r#""a\"b\\c\nd""#);
        assert_eq!(dumps(&json!("\u{1}")), r#""\u0001""#);
        assert_eq!(dumps(&json!("\u{7f}")), r#""\u007f""#);
    }

    #[test]
    fn floats_follow_python_repr() {
        assert_eq!(python_float(1.0), "1.0");
        assert_eq!(python_float(0.5), "0.5");
        assert_eq!(python_float(1729700000.123456), "1729700000.123456");
        assert_eq!(python_float(1e16), "1e+16");
        assert_eq!(python_float(1.5e-7), "1.5e-07");
        assert_eq!(python_float(0.0001), "0.0001");
        assert_eq!(dumps(&json!(10.0)), "10.0");
        assert_eq!(dumps(&json!(3)), "3");
    }

    #[test]
    fn ordered_keeps_insertion_order() {
        assert_eq!(
            dumps_ordered(&[("status", json!("error")), ("error", json!("x"))]),
            r#"{"status": "error", "error": "x"}"#
        );
    }

    #[test]
    fn custom_separators_support_hashing_forms() {
        let value = json!({"a": [1, {"b": 2}]});
        assert_eq!(
            dumps_with_separators(&value, ",", ":"),
            r#"{"a":[1,{"b":2}]}"#
        );
    }
}
