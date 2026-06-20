//! Spork canonical-serialization encoder.
//!
//! A deterministic, **byte-exact** canonical encoding so that identical logical
//! content always hashes identically on any machine — this is the root of
//! content identity for the whole system. Every hash Spork computes (blob,
//! tree, snapshot, `ignore_profile_hash`, the event-log `this_event_hash`
//! chain) is `BLAKE3` over the bytes this crate produces, so the encoding here
//! is load-bearing for the entire content-addressed substrate.
//!
//! # Frozen contract (do not change in place)
//!
//! The byte encoding and [`SERIALIZATION_VERSION`] are **frozen**. Per the
//! project's no-domino rule (C2), a change to the bytes this crate emits is a
//! *loud golden-vector diff* plus a **new generation**, never a silent in-place
//! rehash of existing content. The golden vectors committed under
//! `crates/spork-canon/vectors/` exist precisely to make any such drift fail
//! CI on two independent machines.
//!
//! The canonical form is a strict, deterministic subset of JSON:
//!
//! - **Input** is a [`serde_json::Value`] (or any `T: Serialize`, which is first
//!   serialized to a `Value`). **Output** is UTF-8 bytes.
//! - **Objects**: keys are sorted ascending by their UTF-8 **byte** sequence
//!   (not by Unicode scalar value or locale); each key appears exactly once
//!   (duplicate keys are impossible in a [`serde_json::Map`]); there is no
//!   insignificant whitespace anywhere.
//! - **Strings**: minimal JSON escaping — only `"` (`\"`), `\` (`\\`), and the
//!   C0 control characters `U+0000..=U+001F` are escaped. The control
//!   characters use their short forms where JSON defines them (`\b \t \n \f
//!   \r`) and `\u00XX` otherwise. Every other code point, including all
//!   non-ASCII text, is emitted as literal UTF-8 (it is **not** `\u`-escaped).
//! - **Numbers**: integers only. A number is emitted as a plain decimal with no
//!   leading zeros (other than the single digit `0`), no explicit `+` sign, and
//!   no exponent. **Floating-point numbers are rejected** with
//!   [`CanonError::FloatNotAllowed`]: identity-bearing data must never carry a
//!   float, whose textual round-trip is platform- and library-dependent.
//! - **Booleans / null**: the literals `true`, `false`, `null`.
//! - **Arrays**: element order is preserved exactly.
//!
//! Because the form is a subset of JSON, canonical bytes are always valid JSON
//! and re-parse to a `Value` equal to the input (modulo the float prohibition).
//!
//! # Why not "just" `serde_json`?
//!
//! `serde_json` does not guarantee a stable key order across versions or build
//! configurations, escapes strings more aggressively than we want, and happily
//! emits floats. None of those are acceptable for a hash input that must be
//! reproduced bit-for-bit on any machine, in any future toolchain. This crate
//! pins every one of those degrees of freedom.
//!
//! # Examples
//!
//! ```
//! use spork_canon::canonicalize_value;
//! use serde_json::json;
//!
//! // Keys are byte-sorted and whitespace is removed.
//! let bytes = canonicalize_value(&json!({"b": 1, "a": 2})).unwrap();
//! assert_eq!(bytes, br#"{"a":2,"b":1}"#);
//! ```
//!
//! ```
//! use spork_canon::{canonicalize_value, CanonError};
//! use serde_json::json;
//!
//! // Floats are rejected — identity data must not carry them.
//! assert!(matches!(
//!     canonicalize_value(&json!({"x": 1.5})),
//!     Err(CanonError::FloatNotAllowed)
//! ));
//! ```
//!
//! Design references: DESIGN.md §6.1 (the two-layer content/timeline model and
//! the BLAKE3-everywhere identity decision), Appendix A.1 (the
//! `this_event_hash = H(prev ‖ canonical(payload) ‖ seq)` input-encoder
//! contract and the frozen `serialization_version`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// The frozen version of the canonical-serialization byte encoding.
///
/// This is the `serialization_version` referenced throughout DESIGN.md (§6.1,
/// A.1) and is part of the persisted/hashed identity of any object whose schema
/// records it (e.g. `Snapshot.meta.serialization_version`). Bumping it is a
/// deliberate, generation-introducing event accompanied by a new set of golden
/// vectors — it is **never** changed to "fix" an existing encoding in place.
pub const SERIALIZATION_VERSION: u16 = 1;

/// Errors that can arise while producing canonical bytes.
///
/// The set is intentionally tiny: the only two ways canonicalization can fail
/// are encountering a value that is structurally forbidden in identity-bearing
/// data (a float — [`CanonError::FloatNotAllowed`]) or failing to serialize a
/// caller's `T` into a [`serde_json::Value`] at all
/// ([`CanonError::Serialize`]).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CanonError {
    /// A floating-point number was present in the input.
    ///
    /// Identity-bearing data must contain integers only, because the textual
    /// representation of a float is not byte-stable across platforms and
    /// libraries. Callers that genuinely need fractional quantities must encode
    /// them losslessly as integers (for example, a numerator/denominator pair
    /// or a fixed-point integer with a documented scale) before canonicalizing.
    #[error("floating-point numbers are not allowed in canonical (identity-bearing) data")]
    FloatNotAllowed,

    /// The caller's value could not be serialized into a [`serde_json::Value`].
    ///
    /// This wraps the underlying `serde` error message. It only occurs in
    /// [`canonicalize`], where an arbitrary `T: Serialize` is first converted
    /// to a `Value`; [`canonicalize_value`] cannot produce this variant.
    #[error("failed to serialize value to JSON: {0}")]
    Serialize(String),
}

/// Canonicalize a [`serde_json::Value`] into byte-exact canonical UTF-8 bytes.
///
/// This is the primitive the rest of the workspace builds on: hashing canonical
/// bytes yields a content address. See the [crate-level docs](crate) for the
/// full, frozen set of rules.
///
/// # Errors
///
/// Returns [`CanonError::FloatNotAllowed`] if any number anywhere in the value
/// is a floating-point number (including integers that `serde_json` chose to
/// store as `f64`, such as values produced from a Rust `f64` field). This
/// function never returns [`CanonError::Serialize`].
///
/// # Examples
///
/// ```
/// use spork_canon::canonicalize_value;
/// use serde_json::json;
///
/// let bytes = canonicalize_value(&json!({"z": [3, 2, 1], "a": "café"})).unwrap();
/// assert_eq!(bytes, "{\"a\":\"café\",\"z\":[3,2,1]}".as_bytes());
/// ```
pub fn canonicalize_value(v: &Value) -> Result<Vec<u8>, CanonError> {
    let mut out = Vec::new();
    write_value(v, &mut out)?;
    Ok(out)
}

/// Canonicalize any `T: Serialize` into byte-exact canonical UTF-8 bytes.
///
/// The value is first serialized to a [`serde_json::Value`] and then run
/// through [`canonicalize_value`], so it obeys exactly the same frozen rules.
/// This is the convenient entry point for the typed object model (Blob, Tree,
/// Snapshot, event payloads, …): derive `Serialize` and hand the struct
/// straight to this function.
///
/// # Errors
///
/// Returns [`CanonError::Serialize`] if `T` cannot be represented as a
/// [`serde_json::Value`], and [`CanonError::FloatNotAllowed`] if the resulting
/// value contains any float.
///
/// # Examples
///
/// ```
/// use spork_canon::canonicalize;
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Blob<'a> {
///     v: u16,
///     name: &'a str,
/// }
///
/// let bytes = canonicalize(&Blob { v: 1, name: "x" }).unwrap();
/// assert_eq!(bytes, br#"{"name":"x","v":1}"#);
/// ```
pub fn canonicalize<T: Serialize>(v: &T) -> Result<Vec<u8>, CanonError> {
    let value = serde_json::to_value(v).map_err(|e| CanonError::Serialize(e.to_string()))?;
    canonicalize_value(&value)
}

/// Recursively write a [`Value`] in canonical form into `out`.
///
/// This is the single source of truth for the byte encoding; every public
/// entry point funnels through it.
fn write_value(v: &Value, out: &mut Vec<u8>) -> Result<(), CanonError> {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => write_number(n, out)?,
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sort keys by their raw UTF-8 byte sequence. `String`'s `Ord` is a
            // byte-wise comparison of its UTF-8 bytes, which is exactly the
            // frozen ordering we want, so we can compare the `&String` keys
            // directly without re-encoding.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

            out.push(b'{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                // A key present in `keys` is by construction present in `map`.
                write_value(&map[*key], out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// Write a JSON number in canonical (integer-only) form.
///
/// Accepts only integers. `serde_json` already normalizes integer text (no
/// leading zeros, no `+`, no exponent), so for the integer case we can emit its
/// representation directly; we simply reject anything that is not an integer.
fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), CanonError> {
    // `is_u64`/`is_i64` are true exactly for integer-valued numbers within those
    // ranges. Any number that is neither (i.e. one `serde_json` stored as an
    // `f64`) is a float and is rejected — even if it happens to be integral in
    // value, because its origin was a fractional/`f64` literal whose textual
    // round-trip is not byte-stable.
    if n.is_u64() || n.is_i64() {
        // `Display` for an integer `Number` is plain decimal, no leading zeros,
        // no sign on positives, no exponent: exactly the canonical form.
        out.extend_from_slice(n.to_string().as_bytes());
        Ok(())
    } else {
        Err(CanonError::FloatNotAllowed)
    }
}

/// Write a JSON string with minimal canonical escaping into `out`.
///
/// Only `"`, `\`, and the C0 control characters `U+0000..=U+001F` are escaped;
/// every other code point (including all non-ASCII) is emitted as literal
/// UTF-8. Control characters use their JSON short escapes where defined and
/// lowercase `\u00XX` otherwise.
fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{09}' => out.extend_from_slice(b"\\t"),
            '\u{0A}' => out.extend_from_slice(b"\\n"),
            '\u{0C}' => out.extend_from_slice(b"\\f"),
            '\u{0D}' => out.extend_from_slice(b"\\r"),
            // Remaining C0 controls below 0x20 with no short form: \u00XX.
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let code = c as u32;
                out.extend_from_slice(b"\\u00");
                out.push(HEX[((code >> 4) & 0xF) as usize]);
                out.push(HEX[(code & 0xF) as usize]);
            }
            // Everything else, including all non-ASCII, is literal UTF-8.
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Convenience: canonicalize and decode the result as a UTF-8 `String` for
    /// readable assertions.
    fn canon_str(v: &Value) -> String {
        String::from_utf8(canonicalize_value(v).unwrap()).unwrap()
    }

    #[test]
    fn serialization_version_is_frozen_at_one() {
        assert_eq!(SERIALIZATION_VERSION, 1);
    }

    #[test]
    fn literals() {
        assert_eq!(canon_str(&json!(null)), "null");
        assert_eq!(canon_str(&json!(true)), "true");
        assert_eq!(canon_str(&json!(false)), "false");
    }

    #[test]
    fn integers_plain_decimal() {
        assert_eq!(canon_str(&json!(0)), "0");
        assert_eq!(canon_str(&json!(1)), "1");
        assert_eq!(canon_str(&json!(-1)), "-1");
        assert_eq!(canon_str(&json!(42)), "42");
        // No leading zeros, no plus, no exponent for large magnitudes.
        assert_eq!(canon_str(&json!(i64::MAX)), "9223372036854775807");
        assert_eq!(canon_str(&json!(i64::MIN)), "-9223372036854775808");
        assert_eq!(canon_str(&json!(u64::MAX)), "18446744073709551615");
    }

    #[test]
    fn floats_are_rejected() {
        assert!(matches!(
            canonicalize_value(&json!(1.5)),
            Err(CanonError::FloatNotAllowed)
        ));
        // An integral-valued float still originated as a float -> rejected.
        assert!(matches!(
            canonicalize_value(&json!(2.0)),
            Err(CanonError::FloatNotAllowed)
        ));
        // Nested floats (deep inside objects and arrays) are caught too.
        assert!(matches!(
            canonicalize_value(&json!({"a": {"b": [1, 2, 9.75]}})),
            Err(CanonError::FloatNotAllowed)
        ));
    }

    #[test]
    fn keys_sorted_by_utf8_bytes() {
        assert_eq!(canon_str(&json!({"b": 1, "a": 2})), r#"{"a":2,"b":1}"#);
        // Uppercase sorts before lowercase in byte order ('A'=0x41 < 'a'=0x61).
        assert_eq!(
            canon_str(&json!({"a": 1, "B": 2, "A": 3})),
            r#"{"A":3,"B":2,"a":1}"#
        );
        // Shorter-but-prefix key sorts before its extension ("a" < "ab").
        assert_eq!(canon_str(&json!({"ab": 1, "a": 2})), r#"{"a":2,"ab":1}"#);
    }

    #[test]
    fn nested_objects_sorted_recursively() {
        let v = json!({
            "outer_b": {"y": 1, "x": 2},
            "outer_a": {"d": 3, "c": 4}
        });
        assert_eq!(
            canon_str(&v),
            r#"{"outer_a":{"c":4,"d":3},"outer_b":{"x":2,"y":1}}"#
        );
    }

    #[test]
    fn arrays_preserve_order() {
        assert_eq!(canon_str(&json!([3, 1, 2])), "[3,1,2]");
        assert_eq!(canon_str(&json!([])), "[]");
        // Object members inside arrays are still canonicalized.
        assert_eq!(
            canon_str(&json!([{"b": 1, "a": 2}, {"d": 3, "c": 4}])),
            r#"[{"a":2,"b":1},{"c":4,"d":3}]"#
        );
    }

    #[test]
    fn no_insignificant_whitespace() {
        let v = json!({"a": [1, 2], "b": {"c": 3}});
        let s = canon_str(&v);
        assert!(!s.contains(' '));
        assert!(!s.contains('\n'));
        assert!(!s.contains('\t'));
        assert_eq!(s, r#"{"a":[1,2],"b":{"c":3}}"#);
    }

    #[test]
    fn string_minimal_escaping() {
        // Quote and backslash escaped.
        assert_eq!(canon_str(&json!("a\"b")), r#""a\"b""#);
        assert_eq!(canon_str(&json!("a\\b")), r#""a\\b""#);
        // Short control escapes.
        assert_eq!(canon_str(&json!("\n")), r#""\n""#);
        assert_eq!(canon_str(&json!("\t")), r#""\t""#);
        assert_eq!(canon_str(&json!("\r")), r#""\r""#);
        assert_eq!(canon_str(&json!("\u{08}")), r#""\b""#);
        assert_eq!(canon_str(&json!("\u{0C}")), r#""\f""#);
        // Other C0 controls -> lowercase \u00XX (the backslash is literal output).
        assert_eq!(canon_str(&json!("\u{00}")), "\"\\u0000\"");
        assert_eq!(canon_str(&json!("\u{01}")), "\"\\u0001\"");
        assert_eq!(canon_str(&json!("\u{1F}")), "\"\\u001f\"");
        // Forward slash is NOT escaped (it is not in the escape set).
        assert_eq!(canon_str(&json!("a/b")), r#""a/b""#);
    }

    #[test]
    fn unicode_emitted_as_literal_utf8() {
        // Non-ASCII text is literal UTF-8, never \u-escaped.
        assert_eq!(canon_str(&json!("café")), "\"café\"");
        assert_eq!(canon_str(&json!("日本語")), "\"日本語\"");
        // Emoji (astral plane, surrogate-pair territory in UTF-16) stays literal.
        assert_eq!(canon_str(&json!("😀")), "\"😀\"");
        // Verify the actual bytes for the emoji are the 4-byte UTF-8 sequence.
        let bytes = canonicalize_value(&json!("😀")).unwrap();
        assert_eq!(bytes, [b'"', 0xF0, 0x9F, 0x98, 0x80, b'"']);
        // DEL (0x7F) is >= 0x20, so it is literal, not escaped.
        assert_eq!(
            canonicalize_value(&json!("\u{7F}")).unwrap(),
            [b'"', 0x7F, b'"']
        );
    }

    #[test]
    fn canonicalize_generic_serializes_then_canonicalizes() {
        #[derive(Serialize)]
        struct S {
            v: u16,
            name: String,
            tags: Vec<i64>,
        }
        let s = S {
            v: 1,
            name: "x".to_string(),
            tags: vec![3, 1, 2],
        };
        let bytes = canonicalize(&s).unwrap();
        // Field names are byte-sorted: name < tags < v.
        assert_eq!(bytes, br#"{"name":"x","tags":[3,1,2],"v":1}"#);
    }

    #[test]
    fn canonicalize_generic_rejects_float_field() {
        #[derive(Serialize)]
        struct HasFloat {
            ratio: f64,
        }
        assert!(matches!(
            canonicalize(&HasFloat { ratio: 0.5 }),
            Err(CanonError::FloatNotAllowed)
        ));
    }

    #[test]
    fn deterministic_regardless_of_insertion_order() {
        // Two logically-identical objects built in different orders must produce
        // identical bytes — the whole point of canonicalization.
        let mut a = serde_json::Map::new();
        a.insert("b".into(), json!(2));
        a.insert("a".into(), json!(1));
        a.insert("c".into(), json!(3));

        let mut b = serde_json::Map::new();
        b.insert("c".into(), json!(3));
        b.insert("a".into(), json!(1));
        b.insert("b".into(), json!(2));

        assert_eq!(
            canonicalize_value(&Value::Object(a)).unwrap(),
            canonicalize_value(&Value::Object(b)).unwrap()
        );
    }

    #[test]
    fn output_is_reparseable_valid_json() {
        let v = json!({
            "z": "café \"q\"",
            "a": [1, -2, 3],
            "nested": {"k": null, "j": true}
        });
        let bytes = canonicalize_value(&v).unwrap();
        let reparsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reparsed, v);
    }

    #[test]
    fn idempotent_under_recanonicalization() {
        // Canonicalizing, re-parsing, and canonicalizing again is a no-op.
        let v = json!({"b": {"d": [9, 8], "c": "x\ny"}, "a": 1});
        let once = canonicalize_value(&v).unwrap();
        let reparsed: Value = serde_json::from_slice(&once).unwrap();
        let twice = canonicalize_value(&reparsed).unwrap();
        assert_eq!(once, twice);
    }
}
