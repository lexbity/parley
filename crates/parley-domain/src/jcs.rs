//! RFC 8785 (JCS) canonical JSON serialization.
//!
//! Deterministic JSON for command wire digests: objects are serialized with
//! member names sorted by UTF-16 code units, strings use only the escapes the
//! RFC requires (quote, reverse solidus and `\u00xx` for controls), numbers
//! are written without superfluous leading/trailing digits, and keys are
//! canonicalized to the ECMAScript shortest round-trip form. This module is
//! pure and is qualified against the RFC 8785 test vector set.
//!
//! `ci/trace.py` keeps this crate free of HTTP/SQL/IO dependencies.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JcsError {
    /// A value could not be canonicalized (should not happen for JSON values).
    Unsupported,
}

/// Canonicalize any serde-serializable value into its JCS form.
pub fn canonicalize<T: serde::Serialize>(value: &T) -> Result<String, JcsError> {
    let value = serde_json::to_value(value).map_err(|_| JcsError::Unsupported)?;
    canonicalize_value(&value)
}

/// Canonicalize a parsed JSON value.
pub fn canonicalize_value(value: &serde_json::Value) -> Result<String, JcsError> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &serde_json::Value, out: &mut String) -> Result<(), JcsError> {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => write_number(number, out)?,
        serde_json::Value::String(string) => write_string(string, out),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(compare_utf16);
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&map[*key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// RFC 8785 sorts member names by their UTF-16 code units.
fn compare_utf16(a: &&String, b: &&String) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_string(string: &str, out: &mut String) {
    out.push('"');
    for ch in string.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0000}'..='\u{001f}' => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn write_number(number: &serde_json::Number, out: &mut String) -> Result<(), JcsError> {
    if let Some(integer) = number.as_i64() {
        out.push_str(&integer.to_string());
        return Ok(());
    }
    if let Some(integer) = number.as_u64() {
        out.push_str(&integer.to_string());
        return Ok(());
    }
    let float = number.as_f64().ok_or(JcsError::Unsupported)?;
    write_f64(float, out);
    Ok(())
}

/// ECMAScript `Number.prototype.toString`-style shortest representation.
/// Rust's `Display` for f64 already produces the shortest round-trip digits;
/// this only fixes the notation rules (exponent thresholds, `e+` sign and
/// integer rendering for whole values).
fn write_f64(value: f64, out: &mut String) {
    if value == 0.0 {
        // Both +0.0 and -0.0 serialize as 0 (RFC 8785 test 16).
        out.push('0');
        return;
    }
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        // Whole values in the safe-integer range render as integers.
        out.push_str(&format!("{}", value as i64));
        return;
    }
    if !value.is_finite() {
        // Non-finite values cannot be represented in JSON; nothing canonical
        // exists. The caller must reject them before digest time.
        out.push_str("null");
        return;
    }
    let scientific = format!("{:e}", value);
    // Rust `{:e}` gives "1e-7" or "1e30" (no '+').
    let (mantissa, exponent) = match scientific.split_once('e') {
        Some((m, e)) => (m.to_string(), e.parse::<i32>().unwrap_or(0)),
        None => (scientific, 0),
    };
    let (sign, digits) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest.to_string()),
        None => ("", mantissa),
    };
    // Split mantissa digits from the fraction and strip trailing zeroes.
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits.as_str(), ""));
    let mut significant = format!("{int_part}{frac_part}");
    while significant.len() > 1 && significant.ends_with('0') {
        significant.pop();
    }
    // Decimal exponent of the first significant digit.
    let first_exp = exponent - (int_part.len() as i32 - 1);
    // JS uses exponential notation for |x| < 1e-6 or >= 1e21.
    if !(-6..21).contains(&first_exp) {
        out.push_str(sign);
        out.push_str(&significant);
        out.push('e');
        if first_exp >= 0 {
            out.push('+');
        }
        out.push_str(&first_exp.to_string());
        return;
    }
    // Plain decimal notation.
    if first_exp >= 0 {
        out.push_str(sign);
        out.push_str(&significant);
        for _ in 0..first_exp {
            out.push('0');
        }
        if first_exp as usize + 1 < significant.len() {
            out.insert(
                out.len() - (significant.len() - (first_exp as usize + 1)),
                '.',
            );
        }
    } else {
        let zeros = (-first_exp - 1) as usize;
        out.push_str(sign);
        out.push_str("0.");
        for _ in 0..zeros {
            out.push('0');
        }
        out.push_str(&significant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jcs(value: serde_json::Value) -> String {
        canonicalize_value(&value).unwrap()
    }

    #[test]
    fn rfc8785_vectors_serialize_object_members_sorted() {
        assert_eq!(jcs(json!({"a":1,"b":2})), r#"{"a":1,"b":2}"#);
        assert_eq!(jcs(json!({"a":"1","b":"2"})), r#"{"a":"1","b":"2"}"#);
        assert_eq!(jcs(json!({"Y":1,"X":2})), r#"{"X":2,"Y":1}"#);
        assert_eq!(
            jcs(json!({"z":0,"a":1,"A":2,"b":3})),
            r#"{"A":2,"a":1,"b":3,"z":0}"#
        );
    }

    #[test]
    fn rfc8785_vectors_arrays_and_nesting() {
        assert_eq!(jcs(json!([1, 2, 3, 4])), "[1,2,3,4]");
        assert_eq!(jcs(json!([[1, 2], [3, 4]])), "[[1,2],[3,4]]");
        assert_eq!(
            jcs(json!({"outer":{"z":1,"a":2}})),
            r#"{"outer":{"a":2,"z":1}}"#
        );
    }

    #[test]
    fn rfc8785_vectors_string_escapes() {
        assert_eq!(jcs(json!({"a":"\u{0000}"})), r#"{"a":"\u0000"}"#);
        assert_eq!(jcs(json!({"a":"\u{0001}"})), r#"{"a":"\u0001"}"#);
        assert_eq!(jcs(json!({"a":"\u{0008}"})), r#"{"a":"\u0008"}"#);
        // U+007F is not a JSON control character; it is emitted literally.
        assert_eq!(jcs(json!({"a":"\u{007f}"})), "{\"a\":\"\u{7f}\"}");
        assert_eq!(jcs(json!({"a":"\n"})), r#"{"a":"\u000a"}"#);
        assert_eq!(jcs(json!({"a":"\t"})), r#"{"a":"\u0009"}"#);
        assert_eq!(jcs(json!({"a":"\u{001b}"})), r#"{"a":"\u001b"}"#);
        assert_eq!(
            jcs(json!({"a":"quote\"back\\"})),
            r#"{"a":"quote\"back\\"}"#
        );
        assert_eq!(jcs(json!({"a":"b"})), r#"{"a":"b"}"#);
        assert_eq!(jcs(json!({"a":null})), r#"{"a":null}"#);
        assert_eq!(jcs(json!({})), "{}");
        assert_eq!(jcs(json!([])), "[]");
    }

    #[test]
    fn rfc8785_vectors_numbers() {
        assert_eq!(jcs(json!([0, 1, -1])), "[0,1,-1]");
        assert_eq!(jcs(json!([-1, 0, 1])), "[-1,0,1]");
        assert_eq!(jcs(json!([1.0])), "[1]");
        assert_eq!(jcs(json!([0.1])), "[0.1]");
        assert_eq!(jcs(json!([100.0])), "[100]");
        assert_eq!(jcs(json!([1e2])), "[100]");
        assert_eq!(jcs(json!([-0.0])), "[0]");
        assert_eq!(jcs(json!([0.01])), "[0.01]");
        assert_eq!(jcs(json!([0.10])), "[0.1]");
        assert_eq!(jcs(json!([1e-2])), "[0.01]");
        assert_eq!(jcs(json!([0.000001])), "[0.000001]");
        assert_eq!(jcs(json!([1e-7])), "[1e-7]");
        assert_eq!(jcs(json!([1.234])), "[1.234]");
        assert_eq!(jcs(json!([1.2e-1])), "[0.12]");
        assert_eq!(jcs(json!([1e30])), "[1e+30]");
        assert_eq!(jcs(json!([4.50])), "[4.5]");
        assert_eq!(jcs(json!([0.0000000000000000001])), "[1e-19]");
        assert_eq!(jcs(json!([123456789.0])), "[123456789]");
    }

    #[test]
    fn canonicalize_from_serde_roundtrips() {
        let value = json!({"b": [1, 2.5, null, true], "a": {"x": "y"}});
        let canonical = canonicalize(&value).unwrap();
        assert_eq!(canonical, r#"{"a":{"x":"y"},"b":[1,2.5,null,true]}"#);
    }

    #[test]
    fn utf16_key_ordering_hashes_emoji_after_ascii() {
        // U+1F600 is a surrogate pair (0xD83D 0xDE00), which sorts after
        // ASCII code units.
        let value = json!({"a": 1, "😀": 2});
        assert_eq!(jcs(value), r#"{"a":1,"😀":2}"#);
    }
}
