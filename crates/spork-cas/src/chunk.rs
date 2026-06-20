//! FastCDC content-defined chunking.
//!
//! The sub-file dedup engine (DESIGN.md §10.4): a large or binary file is split
//! into content-defined chunks so a one-region edit re-stores only the chunk(s)
//! that actually changed, leaving the rest reusable. Content-defined boundaries
//! (chosen by a rolling hash over the bytes, not by fixed offsets) are what make
//! this robust to insertions/deletions that would otherwise shift every
//! subsequent fixed-size block.
//!
//! # Frozen parameters (v1)
//!
//! Files at or below [`SINGLE_CHUNK_THRESHOLD`] (1 MiB) are emitted as a *single*
//! chunk — they are small enough that chunking only adds overhead — but still go
//! through the same `[chunk]` shape so every file is a [`crate::object::Blob`]
//! over one or more chunk digests. Larger files are chunked with FastCDC using
//! the parameters [`MIN_SIZE`]/[`AVG_SIZE`]/[`MAX_SIZE`] = 16 KiB / 64 KiB /
//! 256 KiB. These parameters are part of how a blob's chunk list is derived;
//! changing them changes how files dedup (not their *identity* — a file's bytes
//! still hash the same regardless of chunking), so they are pinned here.
//!
//! The chunker is deterministic: identical bytes always yield identical chunk
//! boundaries, on any machine, which is the property dedup depends on.
//!
//! Design references: DESIGN.md §10.4 (FastCDC sub-file dedup), §16 (FastCDC in
//! the stack).

use fastcdc::v2020::FastCDC;

/// Files at or below this size are stored as a single chunk.
///
/// 1 MiB: small files gain nothing from chunking (they would be one chunk
/// anyway given the FastCDC max), and skipping the chunker avoids its setup cost
/// on the overwhelmingly common small-file case in a source tree.
pub const SINGLE_CHUNK_THRESHOLD: usize = 1024 * 1024;

/// FastCDC minimum chunk size: 16 KiB. No cut point is taken before this many
/// bytes into a chunk.
pub const MIN_SIZE: usize = 16 * 1024;
/// FastCDC average (target) chunk size: 64 KiB. The rolling-hash mask is derived
/// from this.
pub const AVG_SIZE: usize = 64 * 1024;
/// FastCDC maximum chunk size: 256 KiB. A cut point is forced at this length even
/// if the rolling hash has not signalled one.
pub const MAX_SIZE: usize = 256 * 1024;

/// A half-open byte range `[start, end)` identifying one chunk within a buffer.
///
/// Returned by [`chunk_ranges`] so the caller can slice the original buffer
/// without the chunker copying bytes. `start..end` always lies within the source
/// buffer and the ranges tile it exactly (contiguous, non-overlapping, covering
/// every byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRange {
    /// Inclusive start offset into the source buffer.
    pub start: usize,
    /// Exclusive end offset into the source buffer.
    pub end: usize,
}

impl ChunkRange {
    /// The length of the chunk in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the chunk is empty (only possible for an empty input buffer).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Split `data` into content-defined chunk ranges.
///
/// - An **empty** buffer yields a single empty range `0..0` (so an empty file is
///   still one chunk and therefore a well-formed blob).
/// - A buffer at or below [`SINGLE_CHUNK_THRESHOLD`] yields a single range
///   covering the whole buffer.
/// - A larger buffer is chunked with FastCDC at [`MIN_SIZE`]/[`AVG_SIZE`]/
///   [`MAX_SIZE`].
///
/// The returned ranges tile `data` exactly and are deterministic for given
/// bytes. The result is never empty.
#[must_use]
pub fn chunk_ranges(data: &[u8]) -> Vec<ChunkRange> {
    if data.is_empty() {
        return vec![ChunkRange { start: 0, end: 0 }];
    }
    if data.len() <= SINGLE_CHUNK_THRESHOLD {
        return vec![ChunkRange {
            start: 0,
            end: data.len(),
        }];
    }

    let chunker = FastCDC::new(data, MIN_SIZE, AVG_SIZE, MAX_SIZE);
    let mut ranges = Vec::new();
    for chunk in chunker {
        ranges.push(ChunkRange {
            start: chunk.offset,
            end: chunk.offset + chunk.length,
        });
    }
    // FastCDC always covers the whole buffer for non-empty input, but guard the
    // invariant defensively: a non-empty buffer must produce at least one chunk.
    debug_assert!(!ranges.is_empty(), "non-empty input must yield chunks");
    debug_assert_eq!(
        ranges.last().map(|r| r.end),
        Some(data.len()),
        "chunks must tile the whole buffer"
    );
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a deterministic pseudo-random buffer of `n` bytes. A simple LCG is
    /// enough to defeat the "all-zeros" degenerate case where FastCDC would emit
    /// max-size chunks only, while staying perfectly reproducible.
    fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            out.push((v >> 33) as u8);
        }
        out
    }

    fn tiles_exactly(data: &[u8], ranges: &[ChunkRange]) {
        assert!(!ranges.is_empty());
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().unwrap().end, data.len());
        for w in ranges.windows(2) {
            assert_eq!(w[0].end, w[1].start, "ranges must be contiguous");
        }
    }

    #[test]
    fn empty_input_is_one_empty_chunk() {
        let ranges = chunk_ranges(&[]);
        assert_eq!(ranges, vec![ChunkRange { start: 0, end: 0 }]);
        assert!(ranges[0].is_empty());
    }

    #[test]
    fn small_input_is_single_chunk() {
        let data = pseudo_random(4096, 1);
        let ranges = chunk_ranges(&data);
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            ranges[0],
            ChunkRange {
                start: 0,
                end: 4096
            }
        );
        tiles_exactly(&data, &ranges);
    }

    #[test]
    fn exactly_threshold_is_single_chunk() {
        let data = pseudo_random(SINGLE_CHUNK_THRESHOLD, 2);
        let ranges = chunk_ranges(&data);
        assert_eq!(ranges.len(), 1);
        tiles_exactly(&data, &ranges);
    }

    #[test]
    fn large_input_is_multi_chunk_and_tiles() {
        let data = pseudo_random(4 * 1024 * 1024, 3);
        let ranges = chunk_ranges(&data);
        assert!(ranges.len() > 1, "4 MiB should produce several chunks");
        tiles_exactly(&data, &ranges);
        // Every chunk respects the configured maximum.
        for r in &ranges {
            assert!(r.len() <= MAX_SIZE);
        }
    }

    #[test]
    fn chunking_is_deterministic() {
        let data = pseudo_random(2 * 1024 * 1024, 4);
        assert_eq!(chunk_ranges(&data), chunk_ranges(&data));
    }

    #[test]
    fn single_region_edit_shifts_only_local_chunks() {
        // The headline property: editing one region of a large buffer leaves most
        // chunk boundaries intact, so most chunk *contents* are byte-identical.
        let mut a = pseudo_random(4 * 1024 * 1024, 5);
        let ra = chunk_ranges(&a);

        // Flip a single byte deep in the middle.
        let mid = a.len() / 2;
        a[mid] ^= 0xFF;
        let rb = chunk_ranges(&a);

        // Reconstruct each side's chunk byte-vectors and count how many are
        // shared. A localized edit must keep the vast majority identical.
        let bytes_a: Vec<Vec<u8>> = ra.iter().map(|r| a[r.start..r.end].to_vec()).collect();
        // (a already mutated; recompute "before" separately)
        let mut before = pseudo_random(4 * 1024 * 1024, 5);
        let ra2 = chunk_ranges(&before);
        let _ = &mut before;
        let bytes_before: Vec<Vec<u8>> = ra2
            .iter()
            .map(|r| before[r.start..r.end].to_vec())
            .collect();
        let bytes_b: Vec<Vec<u8>> = rb.iter().map(|r| a[r.start..r.end].to_vec()).collect();
        let _ = bytes_a;

        let set_before: std::collections::HashSet<&Vec<u8>> = bytes_before.iter().collect();
        let shared = bytes_b.iter().filter(|c| set_before.contains(c)).count();
        // Most chunks must be reused.
        assert!(
            shared >= bytes_b.len().saturating_sub(3),
            "a single-byte edit changed too many chunks: shared {shared} of {}",
            bytes_b.len()
        );
    }
}
