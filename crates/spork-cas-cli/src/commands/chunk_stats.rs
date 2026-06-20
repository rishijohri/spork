//! `chunk-stats`: prove dedup by capturing a directory twice.
//!
//! Captures `dir` cold (into a fresh-or-existing store) and then warm (a second,
//! identical capture). The dedup contract (DESIGN.md §10.4) requires the warm
//! re-capture to write **zero** new objects and chunks — every object is already
//! present, so it is all reuse. This command reports both passes and exits
//! non-zero if the warm pass wrote anything, making it a self-checking DoD demo.
//!
//! Using the *same* store for both passes is the point: cold populates it, warm
//! must find everything already there. The command does not delete the store
//! between runs, so re-invoking it on an unchanged tree is itself a warm pass.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use spork_cas::PutStats;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};

use crate::cli::ChunkStatsArgs;
use crate::commands::ExitCode;
use crate::store::open_store;

/// One capture pass's accounting, in machine-readable form.
#[derive(Debug, Serialize)]
pub struct PassReport {
    /// Higher-level objects newly written.
    pub new_objects: u64,
    /// Higher-level objects reused.
    pub reused_objects: u64,
    /// Leaf chunks newly written.
    pub new_chunks: u64,
    /// Leaf chunks reused.
    pub reused_chunks: u64,
    /// Payload bytes newly persisted.
    pub bytes_written: u64,
    /// Wall-clock duration of the pass, in milliseconds.
    pub elapsed_ms: u128,
}

impl PassReport {
    /// Build a pass report from raw stats and a measured duration.
    fn new(stats: PutStats, elapsed_ms: u128) -> Self {
        PassReport {
            new_objects: stats.new_objects,
            reused_objects: stats.reused_objects,
            new_chunks: stats.new_chunks,
            reused_chunks: stats.reused_chunks,
            bytes_written: stats.bytes_written,
            elapsed_ms,
        }
    }
}

/// The full `chunk-stats` result: both passes plus the dedup verdict.
#[derive(Debug, Serialize)]
pub struct ChunkStatsReport {
    /// The root tree digest (identical across both passes).
    pub root_tree: String,
    /// The cold (first) capture's accounting.
    pub cold: PassReport,
    /// The warm (second) capture's accounting.
    pub warm: PassReport,
    /// Whether the warm pass wrote nothing new (the dedup contract).
    pub deduped: bool,
}

/// Run `chunk-stats`.
///
/// # Errors
/// Returns an error if `dir` is not a directory, the store cannot be opened, or
/// either capture fails. A *failed dedup check* is reported as a non-zero exit
/// code, not an error.
pub fn run(args: ChunkStatsArgs) -> Result<ExitCode> {
    if !args.dir.is_dir() {
        bail!("not a directory: {}", args.dir.display());
    }

    let store = open_store(&args.store.store)?;
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);

    let t0 = Instant::now();
    let (cold_tree, cold_stats) = store
        .put_tree(&args.dir, &matcher)
        .with_context(|| format!("cold capture of {}", args.dir.display()))?;
    let cold_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let (warm_tree, warm_stats) = store
        .put_tree(&args.dir, &matcher)
        .with_context(|| format!("warm capture of {}", args.dir.display()))?;
    let warm_ms = t1.elapsed().as_millis();

    // The two passes must agree on the tree id (pure content addressing).
    debug_assert_eq!(cold_tree, warm_tree);

    let deduped =
        warm_stats.new_objects == 0 && warm_stats.new_chunks == 0 && warm_stats.bytes_written == 0;

    let report = ChunkStatsReport {
        root_tree: warm_tree.to_hex(),
        cold: PassReport::new(cold_stats, cold_ms),
        warm: PassReport::new(warm_stats, warm_ms),
        deduped,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report);
    }

    Ok(if deduped { 0 } else { 1 })
}

/// Print a human-readable two-pass dedup summary.
fn print_human(r: &ChunkStatsReport) {
    println!("root_tree   {}", r.root_tree);
    println!(
        "cold        {} new objects, {} new chunks, {} bytes ({} ms)",
        r.cold.new_objects, r.cold.new_chunks, r.cold.bytes_written, r.cold.elapsed_ms
    );
    println!(
        "warm        {} new objects, {} new chunks, {} bytes ({} ms)",
        r.warm.new_objects, r.warm.new_chunks, r.warm.bytes_written, r.warm.elapsed_ms
    );
    println!(
        "reused warm {} objects, {} chunks",
        r.warm.reused_objects, r.warm.reused_chunks
    );
    if r.deduped {
        println!("dedup       OK — warm re-capture wrote nothing new");
    } else {
        println!("dedup       FAILED — warm re-capture wrote new objects/chunks");
    }
}
