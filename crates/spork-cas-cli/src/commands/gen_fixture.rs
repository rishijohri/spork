//! `gen-fixture`: deterministically create a corpus for the perf / DoD harness.
//!
//! Materializes a reproducible directory of `--files` files (mostly small text,
//! a handful of large binary) seeded by `--seed`, with no wall-clock or OS
//! randomness (see [`crate::fixture`]). The same `(dir, files, seed)` always
//! yields byte-identical files, which is what lets the harness measure cold
//! capture, a one-file edit, and warm re-capture against a stable baseline
//! (DESIGN.md §14.5, A.5).

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::GenFixtureArgs;
use crate::commands::ExitCode;
use crate::fixture::{generate, FixtureSummary};

/// Machine-readable summary of a generated fixture.
#[derive(Debug, Serialize)]
pub struct GenFixtureReport {
    /// The directory the corpus was written to.
    pub dir: String,
    /// Total files written.
    pub files: usize,
    /// How many of those are large binary files.
    pub large_files: usize,
    /// Total bytes written.
    pub bytes: u64,
    /// The seed the corpus was generated from.
    pub seed: u64,
}

impl GenFixtureReport {
    /// Build a report from a fixture summary and the target directory.
    fn new(dir: String, s: FixtureSummary) -> Self {
        GenFixtureReport {
            dir,
            files: s.files,
            large_files: s.large_files,
            bytes: s.bytes,
            seed: s.seed,
        }
    }
}

/// Run `gen-fixture`.
///
/// # Errors
/// Returns an error if the corpus cannot be written.
pub fn run(args: GenFixtureArgs) -> Result<ExitCode> {
    let summary = generate(&args.dir, args.files, args.seed)
        .with_context(|| format!("generating fixture in {}", args.dir.display()))?;

    let report = GenFixtureReport::new(args.dir.display().to_string(), summary);

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("dir         {}", report.dir);
        println!(
            "files       {} ({} large)",
            report.files, report.large_files
        );
        println!("bytes       {}", report.bytes);
        println!("seed        {}", report.seed);
    }
    Ok(0)
}
