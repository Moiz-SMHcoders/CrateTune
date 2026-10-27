// JSON value helpers ported from ArchiveTune
// modules/src/main/kotlin/moe/rukamori/archivetune/modules/ModuleModels.kt
// (jsonValue, parseJsonValue, equivalentJsonValue) — © Rukamori and ArchiveTune contributors, GPL-3.0
//! JSON helpers shared by the manifest, catalog and runtime code.

use serde_json::Value;

/// Deep equality where numbers compare by value (`1` == `1.0`).
pub(crate) fn equivalent(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equivalent(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| equivalent(v, w)))
        }
        _ => a == b,
    }
}

/// Object/array nesting depth (a scalar is 0).
pub(crate) fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// A string or number field as text (`""` otherwise), like the reference's `text()`.
pub(crate) fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// A string field (`""` for anything else), like `JSONObject.optString` for strings.
pub(crate) fn opt_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Nonblank strings of an array field (non-strings and blanks dropped).
pub(crate) fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A number field as f64 if it is finite.
pub(crate) fn finite(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

/// Integer-valued JSON number (`1`, not `1.0` or `"1"`).
pub(crate) fn is_int(value: Option<&Value>, expected: i64) -> bool {
    matches!(value, Some(Value::Number(n)) if n.as_i64() == Some(expected) && !n.is_f64())
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

pub(crate) fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // Practically unreachable; fall back to time + counter entropy.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        b.copy_from_slice(&nanos.to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn equivalence_depth_and_ids() {
        assert!(equivalent(&json!(1), &json!(1.0)));
        assert!(equivalent(
            &json!({"a": [1, "x"]}),
            &json!({"a": [1.0, "x"]})
        ));
        assert!(!equivalent(&json!("1"), &json!(1)));
        assert_eq!(depth(&json!({"a": [[1]]})), 3);
        assert!(is_int(Some(&json!(1)), 1));
        assert!(!is_int(Some(&json!(1.0)), 1));
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_ne!(id, uuid_v4());
        assert!(is_sha256_hex(&sha256_hex(b"x")));
    }
}
