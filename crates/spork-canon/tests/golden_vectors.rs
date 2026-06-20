//! Golden-vector test for the frozen canonical encoding.
//!
//! Each case under `crates/spork-canon/vectors/` is a pair:
//!
//! - `NN_name.json`      — a human-readable JSON input document, and
//! - `NN_name.expected`  — the **exact** canonical bytes the encoder must emit.
//!
//! This test parses every `*.json`, canonicalizes it through the crate, and
//! asserts the result is byte-identical to the committed `*.expected`. Because
//! the encoding is frozen (DESIGN.md §6.1, A.1), any drift in the byte output
//! shows up here as a loud diff on *two independent machines* — which is the
//! Definition-of-Done check that canonicalization vectors are byte-identical
//! across machines.
//!
//! ## Regenerating the expected files
//!
//! The `*.expected` files are produced by the encoder itself (it is the single
//! source of truth for the bytes). To (re)generate them after an *intentional,
//! generation-introducing* change, run:
//!
//! ```text
//! SPORK_CANON_REGEN=1 cargo test -p spork-canon --test golden_vectors
//! ```
//!
//! Regeneration writes the files and then still asserts they round-trip. Under
//! normal CI runs the env var is unset, so the committed bytes are treated as
//! frozen and only verified, never rewritten.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use spork_canon::canonicalize_value;

/// Absolute path to the `vectors/` directory, resolved from this crate's root.
fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors")
}

/// Collect the sorted list of `*.json` input files in `vectors/`.
fn input_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(vectors_dir())
        .expect("vectors/ directory must exist")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "expected at least one *.json vector");
    files
}

#[test]
fn golden_vectors_are_byte_identical() {
    let regen = std::env::var_os("SPORK_CANON_REGEN").is_some();

    let inputs = input_files();
    // Guard against accidentally deleting coverage: we expect the full suite.
    assert!(
        inputs.len() >= 7,
        "expected the full golden-vector suite (>= 7 cases), found {}",
        inputs.len()
    );

    for input_path in inputs {
        let raw = fs::read_to_string(&input_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", input_path.display()));
        let value: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse {}: {e}", input_path.display()));

        let actual = canonicalize_value(&value)
            .unwrap_or_else(|e| panic!("canonicalize {}: {e}", input_path.display()));

        let expected_path = input_path.with_extension("expected");

        if regen {
            fs::write(&expected_path, &actual)
                .unwrap_or_else(|e| panic!("write {}: {e}", expected_path.display()));
        }

        let expected = fs::read(&expected_path).unwrap_or_else(|e| {
            panic!(
                "read {} (run with SPORK_CANON_REGEN=1 to generate): {e}",
                expected_path.display()
            )
        });

        assert_eq!(
            actual,
            expected,
            "canonical bytes for {} do not match the frozen golden vector {}.\n\
             If this change is intentional it is a NEW GENERATION: bump \
             SERIALIZATION_VERSION and regenerate with SPORK_CANON_REGEN=1.",
            input_path.display(),
            expected_path.display()
        );
    }
}

#[test]
fn golden_outputs_reparse_to_input_value() {
    // The canonical output is always valid JSON that re-parses to a value equal
    // to the (float-free) input — a cross-check that the golden bytes are not
    // merely stable but semantically faithful.
    for input_path in input_files() {
        let raw = fs::read_to_string(&input_path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let canon = canonicalize_value(&value).unwrap();
        let reparsed: Value = serde_json::from_slice(&canon)
            .unwrap_or_else(|e| panic!("reparse canonical {}: {e}", input_path.display()));
        assert_eq!(
            reparsed,
            value,
            "round-trip mismatch for {}",
            input_path.display()
        );
    }
}

#[test]
fn golden_outputs_are_idempotent() {
    // Canonicalizing an already-canonical document is a no-op: canon(parse(canon(x))) == canon(x).
    for input_path in input_files() {
        let raw = fs::read_to_string(&input_path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let once = canonicalize_value(&value).unwrap();
        let reparsed: Value = serde_json::from_slice(&once).unwrap();
        let twice = canonicalize_value(&reparsed).unwrap();
        assert_eq!(
            once,
            twice,
            "non-idempotent canonicalization for {}",
            input_path.display()
        );
    }
}
