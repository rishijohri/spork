//! Deterministic fixture generation for the perf / definition-of-done harness.
//!
//! The DoD demands a reproducible 50k-file corpus to measure incremental
//! snapshot capture against (DESIGN.md §14.5, A.5). "Reproducible" is the load-
//! bearing word: the generated tree must be **byte-identical on every run and
//! every machine** so that a cold capture, an edit, and a warm re-capture are
//! comparing like with like. This module therefore admits **no wall-clock and no
//! OS randomness** — every byte is a pure function of the caller's `seed` and the
//! file's index, via a small splitmix64 PRNG ([`Prng`]).
//!
//! The corpus is a mix that exercises the whole store:
//! - many small UTF-8 text files spread across a bounded directory fan-out, so
//!   the directory walk and [`spork_cas::Tree`] building are exercised at scale;
//! - a handful of larger binary files (a few MiB each) so FastCDC content-defined
//!   chunking and sub-file dedup have something to bite on.
//!
//! Design references: DESIGN.md §14.5 (selection / perf budgets), §10.4 (FastCDC
//! sub-file dedup), Appendix A.5 (measured budgets the harness validates).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The default number of leaf files generated when `--files` is not supplied.
pub const DEFAULT_FILE_COUNT: usize = 1_000;

/// The number of files placed in each leaf directory before opening a new one.
///
/// A bounded fan-out keeps any single directory small (mirroring real projects
/// and the `objects/<aa>` fan-out the store itself uses) while still producing a
/// deep, branchy tree from a large file count.
const FILES_PER_DIR: usize = 64;

/// How many of the generated files are "large" binary files (the FastCDC bait).
///
/// Kept small and absolute (not a fraction) so the corpus size scales gently:
/// the perf harness cares about *file count*, and multi-MiB files are expensive
/// to write, so a fixed handful is enough to prove sub-file dedup.
const LARGE_FILE_COUNT: usize = 4;

/// The size, in bytes, of each generated large binary file (4 MiB).
///
/// Comfortably above the store's single-chunk threshold so FastCDC produces many
/// chunks and a one-region edit re-stores exactly one of them.
const LARGE_FILE_SIZE: usize = 4 * 1024 * 1024;

/// A summary of what [`generate`] wrote, for the CLI to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixtureSummary {
    /// Total files written (small text files plus large binary files).
    pub files: usize,
    /// How many of those are large binary files.
    pub large_files: usize,
    /// Total bytes written across all files.
    pub bytes: u64,
    /// The seed the corpus was generated from (echoed for reproducibility).
    pub seed: u64,
}

/// A tiny, fast, fully deterministic PRNG (splitmix64).
///
/// Used instead of the `rand` crate precisely because the fixture must be
/// reproducible from a seed alone — no entropy source, no global state. Seeding
/// it with `seed ^ index` per file makes each file independently reproducible
/// while still varying across the corpus.
struct Prng(u64);

impl Prng {
    /// Seed the generator. Any `u64` is a valid seed.
    fn new(seed: u64) -> Self {
        Prng(seed)
    }

    /// Advance the state and return the next pseudo-random `u64`.
    fn next_u64(&mut self) -> u64 {
        // splitmix64: a well-known, high-quality finalizer over a Weyl sequence.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Fill `buf` with deterministic pseudo-random bytes.
    fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_exact_mut(8);
        for chunk in &mut chunks {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        let rem = chunks.into_remainder();
        if !rem.is_empty() {
            let bytes = self.next_u64().to_le_bytes();
            rem.copy_from_slice(&bytes[..rem.len()]);
        }
    }
}

/// Generate a deterministic corpus of `files` files rooted at `root`, seeded by
/// `seed`.
///
/// Creates `root` (and the directory fan-out beneath it) and writes:
/// - `files - LARGE_FILE_COUNT` small UTF-8 text files (clamped so at least one
///   small file is written whenever `files > 0`), each whose content is a pure
///   function of `(seed, index)`;
/// - up to [`LARGE_FILE_COUNT`] large binary files (fewer if `files` is tiny),
///   each [`LARGE_FILE_SIZE`] bytes of seeded pseudo-random data.
///
/// Re-running with the same `(root, files, seed)` reproduces byte-identical
/// content, which is what lets the harness diff cold/warm captures meaningfully.
///
/// # Errors
/// Returns any [`io::Error`] from creating directories or writing files.
pub fn generate(root: &Path, files: usize, seed: u64) -> io::Result<FixtureSummary> {
    fs::create_dir_all(root)?;

    // Decide how many large vs. small files to write. Large files only appear
    // once the corpus is big enough to warrant them; tiny corpora are all-small.
    let large = LARGE_FILE_COUNT.min(files / 2);
    let small = files - large;

    let mut bytes_written: u64 = 0;

    // Small text files, spread across a bounded directory fan-out.
    for index in 0..small {
        let dir = root.join(leaf_dir_name(index));
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("file_{index:06}.txt"));
        let content = small_file_contents(seed, index);
        fs::write(&path, &content)?;
        bytes_written += content.len() as u64;
    }

    // Large binary files in a dedicated directory (kept out of the small-file
    // fan-out so the harness can target them for the "edit one file" demo).
    if large > 0 {
        let big_dir = root.join("large");
        fs::create_dir_all(&big_dir)?;
        for index in 0..large {
            let path = big_dir.join(format!("blob_{index:03}.bin"));
            let content = large_file_contents(seed, index);
            fs::write(&path, &content)?;
            bytes_written += content.len() as u64;
        }
    }

    Ok(FixtureSummary {
        files: small + large,
        large_files: large,
        bytes: bytes_written,
        seed,
    })
}

/// The relative leaf-directory path for the small file at `index`.
///
/// Files are bucketed `FILES_PER_DIR` at a time into `dir_NNNN` directories so no
/// single directory grows without bound.
fn leaf_dir_name(index: usize) -> PathBuf {
    let bucket = index / FILES_PER_DIR;
    PathBuf::from(format!("dir_{bucket:04}"))
}

/// Deterministic textual contents for the small file at `index`.
///
/// A short, human-readable, UTF-8 body whose every line is derived from
/// `(seed, index)`, including a non-ASCII marker so unicode handling is on the
/// path. Returned as bytes (already valid UTF-8) for a uniform write path.
fn small_file_contents(seed: u64, index: usize) -> Vec<u8> {
    let mut rng = Prng::new(seed ^ (index as u64).wrapping_mul(0x100_0000_01B3));
    // A handful of lines, count and content seeded so files differ but are
    // reproducible.
    let lines = 3 + (rng.next_u64() % 6) as usize;
    let mut s = String::new();
    s.push_str(&format!("// spork fixture file {index} (seed {seed})\n"));
    s.push_str("// deterministic content — reproducible across machines ✓\n");
    for line in 0..lines {
        let token = rng.next_u64();
        s.push_str(&format!("line {line}: token={token:016x} λ\n"));
    }
    s.into_bytes()
}

/// Deterministic binary contents for the large file at `index`.
///
/// Seeded pseudo-random bytes of [`LARGE_FILE_SIZE`] length so the file is large
/// enough to drive FastCDC into many chunks.
fn large_file_contents(seed: u64, index: usize) -> Vec<u8> {
    let mut rng = Prng::new(seed ^ 0xA5A5_A5A5_0000_0000 ^ index as u64);
    let mut buf = vec![0u8; LARGE_FILE_SIZE];
    rng.fill(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                collect(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v = Vec::new();
        collect(root, root, &mut v);
        v.sort();
        v
    }

    #[test]
    fn prng_is_deterministic() {
        let mut a = Prng::new(42);
        let mut b = Prng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        // Different seeds diverge.
        let mut c = Prng::new(43);
        assert_ne!(Prng::new(42).next_u64(), c.next_u64());
    }

    #[test]
    fn fill_handles_non_multiple_of_eight() {
        let mut rng = Prng::new(7);
        let mut buf = vec![0u8; 13];
        rng.fill(&mut buf);
        // A second fill from the same seed reproduces the bytes exactly.
        let mut rng2 = Prng::new(7);
        let mut buf2 = vec![0u8; 13];
        rng2.fill(&mut buf2);
        assert_eq!(buf, buf2);
    }

    #[test]
    fn same_seed_produces_byte_identical_tree() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let sa = generate(a.path(), 200, 1234).unwrap();
        let sb = generate(b.path(), 200, 1234).unwrap();
        assert_eq!(sa, sb);
        assert_eq!(snapshot(a.path()), snapshot(b.path()));
    }

    #[test]
    fn different_seed_produces_different_tree() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        generate(a.path(), 200, 1).unwrap();
        generate(b.path(), 200, 2).unwrap();
        assert_ne!(snapshot(a.path()), snapshot(b.path()));
    }

    #[test]
    fn regeneration_in_place_is_idempotent() {
        // Writing the same fixture twice into the same dir leaves identical bytes.
        let dir = TempDir::new().unwrap();
        generate(dir.path(), 150, 99).unwrap();
        let first = snapshot(dir.path());
        generate(dir.path(), 150, 99).unwrap();
        let second = snapshot(dir.path());
        assert_eq!(first, second);
    }

    #[test]
    fn small_corpus_writes_only_small_files() {
        let dir = TempDir::new().unwrap();
        let summary = generate(dir.path(), 3, 0).unwrap();
        assert_eq!(summary.files, 3);
        assert_eq!(summary.large_files, 1); // files/2 == 1
        assert!(!dir.path().join("large").join("blob_001.bin").exists());
    }

    #[test]
    fn large_files_exceed_single_chunk_threshold() {
        let dir = TempDir::new().unwrap();
        let summary = generate(dir.path(), 50, 5).unwrap();
        assert_eq!(summary.large_files, LARGE_FILE_COUNT);
        let big = fs::read(dir.path().join("large").join("blob_000.bin")).unwrap();
        assert!(big.len() > spork_cas::SINGLE_CHUNK_THRESHOLD);
    }

    #[test]
    fn zero_files_is_an_empty_but_valid_corpus() {
        let dir = TempDir::new().unwrap();
        let summary = generate(dir.path(), 0, 0).unwrap();
        assert_eq!(summary.files, 0);
        assert_eq!(summary.bytes, 0);
        assert!(dir.path().exists());
    }
}
