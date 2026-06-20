//! `verify-roundtrip`: capture a directory, re-materialize it, and compare.
//!
//! Proves the store's roundtrip-fidelity invariant (DESIGN.md §10.1): capturing
//! a tree and materializing it back must reproduce every non-ignored file
//! **byte-for-byte**. The command captures `dir` under the default ignore
//! profile, materializes the resulting tree into a fresh temp directory, then
//! walks both sides and asserts the set of `(relative path, bytes)` for kept
//! files is identical. It exits 0 when the roundtrip holds and 1 when it does
//! not, so it is a self-checking DoD demo.
//!
//! Ignored entries are excluded from *both* sides of the comparison: they never
//! enter the store (so they cannot be materialized), and the source-side walk
//! applies the same matcher, so their absence is expected rather than a failure.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use tempfile::TempDir;

use crate::cli::VerifyRoundtripArgs;
use crate::commands::ExitCode;
use crate::store::open_store;

/// The machine-readable verdict of a roundtrip check.
#[derive(Debug, Serialize)]
pub struct RoundtripReport {
    /// The captured root tree digest (hex).
    pub root_tree: String,
    /// How many non-ignored files were compared.
    pub files_checked: usize,
    /// Whether every compared file matched byte-for-byte.
    pub ok: bool,
    /// Relative paths whose bytes differed or that were missing on one side.
    pub mismatches: Vec<String>,
}

/// Run `verify-roundtrip`.
///
/// # Errors
/// Returns an error if `dir` is not a directory, the store cannot be opened, or
/// capture/materialize fails. A *roundtrip mismatch* is reported via a non-zero
/// exit code (and the `mismatches` list), not as an error.
pub fn run(args: VerifyRoundtripArgs) -> Result<ExitCode> {
    if !args.dir.is_dir() {
        bail!("not a directory: {}", args.dir.display());
    }

    let store = open_store(&args.store.store)?;
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);

    let (root_tree, _stats) = store
        .put_tree(&args.dir, &matcher)
        .with_context(|| format!("capturing {}", args.dir.display()))?;

    let dest = TempDir::new().context("creating temp dir for materialization")?;
    store
        .materialize_tree(&root_tree, dest.path())
        .with_context(|| format!("materializing tree {root_tree}"))?;

    // Collect kept files from the source (applying the same ignore matcher) and
    // every file from the materialized destination.
    let mut source = BTreeMap::new();
    collect_kept(&args.dir, &args.dir, &matcher, &mut source)
        .context("walking source directory")?;
    let mut restored = BTreeMap::new();
    collect_all(dest.path(), dest.path(), &mut restored)
        .context("walking materialized directory")?;

    let mismatches = diff(&source, &restored);
    let ok = mismatches.is_empty();

    let report = RoundtripReport {
        root_tree: root_tree.to_hex(),
        files_checked: source.len(),
        ok,
        mismatches,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report);
    }

    Ok(if ok { 0 } else { 1 })
}

/// Compute the symmetric difference between captured-source and restored files.
///
/// Returns a sorted list of relative paths that are present-on-one-side-only or
/// whose bytes differ. An empty result means the roundtrip is byte-identical.
fn diff(source: &BTreeMap<String, Vec<u8>>, restored: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (path, bytes) in source {
        match restored.get(path) {
            Some(other) if other == bytes => {}
            Some(_) => mismatches.push(format!("{path} (bytes differ)")),
            None => mismatches.push(format!("{path} (missing in restored)")),
        }
    }
    for path in restored.keys() {
        if !source.contains_key(path) {
            mismatches.push(format!("{path} (unexpected in restored)"));
        }
    }
    mismatches.sort();
    mismatches
}

/// Recursively collect every regular file under `dir` whose path (relative to
/// `base`) the `matcher` keeps, into `out` keyed by relative path.
///
/// Mirrors the store's own walk semantics: directories and files are tested
/// against the matcher on their path relative to the capture root, so the kept
/// set matches exactly what `put_tree` stored.
fn collect_kept(
    base: &Path,
    dir: &Path,
    matcher: &IgnoreMatcher,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .expect("child is under base")
            .to_string_lossy()
            .into_owned();
        let meta = std::fs::symlink_metadata(&path)?;
        let is_dir = meta.is_dir();
        if matcher.is_ignored(Path::new(&rel), is_dir) {
            continue;
        }
        if is_dir {
            collect_kept(base, &path, matcher, out)?;
        } else if meta.is_file() {
            out.insert(rel, std::fs::read(&path)?);
        }
        // Symlinks are intentionally not compared by bytes here: the store
        // reconstructs them as links, and the materialized side would read the
        // link target's bytes, not the link itself. Regular-file fidelity is the
        // contract this command verifies.
    }
    Ok(())
}

/// Collect every regular file under `dir` into `out`, keyed by path relative to
/// `base`.
///
/// No ignore filtering is applied — the materialized tree contains only kept
/// entries already, so a plain [`walkdir`] traversal is exactly right here.
fn collect_all(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .expect("child is under base")
            .to_string_lossy()
            .into_owned();
        out.insert(rel, std::fs::read(path)?);
    }
    Ok(())
}

/// Print a human-readable roundtrip verdict.
fn print_human(r: &RoundtripReport) {
    println!("root_tree     {}", r.root_tree);
    println!("files_checked {}", r.files_checked);
    if r.ok {
        println!("roundtrip     OK — all files byte-identical");
    } else {
        println!(
            "roundtrip     FAILED — {} mismatch(es):",
            r.mismatches.len()
        );
        for m in &r.mismatches {
            println!("  {m}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_no_mismatches_when_equal() {
        let mut a = BTreeMap::new();
        a.insert("x".to_string(), vec![1, 2, 3]);
        let mut b = BTreeMap::new();
        b.insert("x".to_string(), vec![1, 2, 3]);
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn diff_reports_byte_differences_and_missing_and_extra() {
        let mut a = BTreeMap::new();
        a.insert("same".to_string(), vec![1]);
        a.insert("changed".to_string(), vec![1]);
        a.insert("only_a".to_string(), vec![9]);
        let mut b = BTreeMap::new();
        b.insert("same".to_string(), vec![1]);
        b.insert("changed".to_string(), vec![2]);
        b.insert("only_b".to_string(), vec![9]);
        let d = diff(&a, &b);
        assert_eq!(
            d,
            vec![
                "changed (bytes differ)".to_string(),
                "only_a (missing in restored)".to_string(),
                "only_b (unexpected in restored)".to_string(),
            ]
        );
    }
}
