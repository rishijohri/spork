//! `cat`: write a stored object to stdout.
//!
//! Given an object digest, emit its content: a [`spork_cas::Blob`] is
//! reassembled into its original file bytes and written raw to stdout; a
//! [`spork_cas::Tree`] or [`spork_cas::Snapshot`] is decoded and pretty-printed
//! as JSON. A raw chunk (or any payload that is not a recognized JSON object) is
//! written verbatim.
//!
//! Kind detection is by structure: the store's [`spork_cas::StorageBackend`]
//! returns only the payload bytes, so `cat` inspects the decoded JSON's shape
//! (blobs carry `chunks`, trees carry `entries`, snapshots carry `root_tree`).
//! `--kind` forces a specific interpretation when the caller already knows it.

use std::io::{self, Write};

use anyhow::{anyhow, Context, Result};
use spork_cas::{LooseStore, ObjectStore, StorageBackend};
use spork_hash::Hash;

use crate::cli::{CatArgs, ObjectKindArg};
use crate::commands::ExitCode;
use crate::store::open_store;

/// Run `cat`.
///
/// # Errors
/// Returns an error if the hash is malformed, the object is absent, or it cannot
/// be decoded as the requested/detected kind.
pub fn run(args: CatArgs) -> Result<ExitCode> {
    let hash: Hash = args
        .hash
        .parse()
        .map_err(|e| anyhow!("invalid object hash {:?}: {e}", args.hash))?;

    let store = open_store(&args.store.store)?;

    // Fetch the raw payload once so we can both detect the kind and (for a
    // non-object payload) emit it verbatim without a second read.
    let payload = store
        .backend()
        .get(&hash)
        .with_context(|| format!("reading object {hash}"))?
        .ok_or_else(|| anyhow!("object not found: {hash}"))?;

    let kind = match args.kind {
        Some(k) => k,
        None => match detect_kind(&payload) {
            Some(k) => k,
            // Not a recognized JSON object (e.g. a raw chunk): emit verbatim.
            None => return emit_raw(&payload).map(|()| 0),
        },
    };

    emit(&store, &hash, kind)?;
    Ok(0)
}

/// Detect an object's kind from its JSON payload shape.
///
/// Returns `None` if the bytes are not a JSON object carrying one of the known
/// discriminating fields (so the caller can fall back to raw output).
fn detect_kind(payload: &[u8]) -> Option<ObjectKindArg> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let obj = value.as_object()?;
    // Order matters: snapshot is checked first because it is the most specific
    // (it alone has `root_tree`), then tree (`entries`), then blob (`chunks`).
    if obj.contains_key("root_tree") {
        Some(ObjectKindArg::Snapshot)
    } else if obj.contains_key("entries") {
        Some(ObjectKindArg::Tree)
    } else if obj.contains_key("chunks") {
        Some(ObjectKindArg::Blob)
    } else {
        None
    }
}

/// Emit an object of a known kind: blob bytes raw, tree/snapshot as pretty JSON.
fn emit(store: &ObjectStore<LooseStore>, hash: &Hash, kind: ObjectKindArg) -> Result<()> {
    match kind {
        ObjectKindArg::Blob => {
            let bytes = store
                .read_blob(hash)
                .with_context(|| format!("reassembling blob {hash}"))?;
            emit_raw(&bytes)
        }
        ObjectKindArg::Tree => {
            let tree = store
                .read_tree(hash)
                .with_context(|| format!("decoding tree {hash}"))?;
            emit_json(&tree)
        }
        ObjectKindArg::Snapshot => {
            let snap = store
                .read_snapshot(hash)
                .with_context(|| format!("decoding snapshot {hash}"))?;
            emit_json(&snap)
        }
    }
}

/// Write raw bytes to stdout (used for blob/chunk content).
fn emit_raw(bytes: &[u8]) -> Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(bytes).context("writing to stdout")?;
    lock.flush().context("flushing stdout")?;
    Ok(())
}

/// Pretty-print a serializable object as JSON followed by a newline.
fn emit_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("serializing object to JSON")?;
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    writeln!(lock, "{json}").context("writing to stdout")?;
    lock.flush().context("flushing stdout")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_kind_distinguishes_object_shapes() {
        let blob = br#"{"v":1,"chunks":["aa"]}"#;
        let tree = br#"{"v":1,"entries":[]}"#;
        let snap = br#"{"v":1,"root_tree":"aa","ignore_profile_hash":"bb","git_parent_commit":null,"meta":{"serialization_version":1}}"#;
        assert_eq!(detect_kind(blob), Some(ObjectKindArg::Blob));
        assert_eq!(detect_kind(tree), Some(ObjectKindArg::Tree));
        assert_eq!(detect_kind(snap), Some(ObjectKindArg::Snapshot));
    }

    #[test]
    fn detect_kind_returns_none_for_raw_bytes() {
        assert_eq!(detect_kind(b"not json at all \x00\xff"), None);
        // Valid JSON that is not one of our objects also falls through to raw.
        assert_eq!(detect_kind(br#"{"unrelated":true}"#), None);
        assert_eq!(detect_kind(br#"[1,2,3]"#), None);
    }
}
