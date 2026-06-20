//! `put-tree`: capture a directory into a snapshot and report dedup stats.
//!
//! Walks `dir` under the frozen default ignore profile (deps excluded by
//! default, DESIGN.md §10.5), stores the resulting blobs/chunks/trees, binds a
//! [`spork_cas::Snapshot`] over the root tree, and prints the snapshot id plus
//! the [`spork_cas::PutStats`] and elapsed wall time. `--json` emits a stable
//! machine-readable record for the perf / DoD harness.
//!
//! Note that wall-clock timing is *reported* but never enters object identity —
//! a snapshot's hash depends only on captured content and the ignore profile
//! (DESIGN.md §6.1), so two timed captures of the same tree share an id.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};

use crate::cli::PutTreeArgs;
use crate::commands::ExitCode;
use crate::store::open_store;

/// The machine-readable result of a `put-tree` capture.
#[derive(Debug, Serialize)]
pub struct PutTreeReport {
    /// The snapshot object's digest (hex).
    pub snapshot: String,
    /// The root tree object's digest (hex).
    pub root_tree: String,
    /// The `ignore_profile_hash` baked into the snapshot's identity (hex).
    pub ignore_profile_hash: String,
    /// Higher-level objects (blobs/trees/snapshot) newly written.
    pub new_objects: u64,
    /// Higher-level objects found already present (reused).
    pub reused_objects: u64,
    /// Leaf chunk objects newly written.
    pub new_chunks: u64,
    /// Leaf chunk objects found already present (reused).
    pub reused_chunks: u64,
    /// Total payload bytes newly persisted.
    pub bytes_written: u64,
    /// Wall-clock duration of the capture, in milliseconds (informational only).
    pub elapsed_ms: u128,
}

/// Run `put-tree`.
///
/// # Errors
/// Returns an error if `dir` is not a directory, the store cannot be opened, or
/// the capture fails.
pub fn run(args: PutTreeArgs) -> Result<ExitCode> {
    if !args.dir.is_dir() {
        bail!("not a directory: {}", args.dir.display());
    }
    validate_git_parent(args.git_parent.as_deref())?;

    let store = open_store(&args.store.store)?;
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);
    let ignore_profile_hash = profile.hash();

    let start = Instant::now();
    let (snapshot, root_tree, stats) = store
        .capture_snapshot(
            &args.dir,
            &matcher,
            ignore_profile_hash,
            args.git_parent.clone(),
        )
        .with_context(|| format!("capturing {}", args.dir.display()))?;
    let elapsed_ms = start.elapsed().as_millis();

    let report = PutTreeReport {
        snapshot: snapshot.to_hex(),
        root_tree: root_tree.to_hex(),
        ignore_profile_hash: ignore_profile_hash.to_hex(),
        new_objects: stats.new_objects,
        reused_objects: stats.reused_objects,
        new_chunks: stats.new_chunks,
        reused_chunks: stats.reused_chunks,
        bytes_written: stats.bytes_written,
        elapsed_ms,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report);
    }
    Ok(0)
}

/// Validate that an imported Git parent is a 40-char lowercase hex SHA-1.
///
/// Spork imports Git SHA-1s but never computes them; we still reject obviously
/// malformed input so a typo doesn't silently land in a snapshot's identity.
fn validate_git_parent(parent: Option<&str>) -> Result<()> {
    if let Some(p) = parent {
        let is_lower_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        let ok = p.len() == 40 && p.bytes().all(is_lower_hex);
        if !ok {
            bail!("--git-parent must be 40 lowercase hex characters (a Git SHA-1), got {p:?}");
        }
    }
    Ok(())
}

/// Print a human-readable capture summary.
fn print_human(r: &PutTreeReport) {
    println!("snapshot           {}", r.snapshot);
    println!("root_tree          {}", r.root_tree);
    println!("ignore_profile     {}", r.ignore_profile_hash);
    println!(
        "objects            {} new, {} reused",
        r.new_objects, r.reused_objects
    );
    println!(
        "chunks             {} new, {} reused",
        r.new_chunks, r.reused_chunks
    );
    println!("bytes_written      {}", r.bytes_written);
    println!("elapsed_ms         {}", r.elapsed_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_parent_validation() {
        assert!(validate_git_parent(None).is_ok());
        assert!(validate_git_parent(Some(&"a".repeat(40))).is_ok());
        assert!(validate_git_parent(Some("0123456789abcdef0123456789abcdef01234567")).is_ok());
        // Too short / too long / uppercase / non-hex are rejected.
        assert!(validate_git_parent(Some("abc")).is_err());
        assert!(validate_git_parent(Some(&"a".repeat(41))).is_err());
        assert!(validate_git_parent(Some(&"A".repeat(40))).is_err());
        assert!(validate_git_parent(Some(&"g".repeat(40))).is_err());
    }
}
