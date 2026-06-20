//! The generic projection driver: [`ProjectionStore`].
//!
//! `ProjectionStore<P>` ties a [`Projection`] to the event log. It does three
//! things, all purely in terms of the trait seam:
//!
//! - [`rebuild_from_log`] folds the *entire* log from `seq 1` into a fresh
//!   projection — the from-scratch path that proves a projection owns no truth
//!   (DESIGN.md A.6).
//! - [`checkpoint`] captures a projection's state at a sequence as a canonical,
//!   hash-pinned [`Checkpoint`].
//! - [`load_or_rebuild`] restores from a checkpoint (if supplied and valid) and
//!   replays only the events *after* it, falling back to a full rebuild
//!   otherwise. Restoring-then-replaying must equal a full rebuild bit-for-bit.
//!
//! The store is a zero-sized type parameterized by `P`; it holds no state of its
//! own (the projection and the log do), so its methods are effectively
//! namespaced constructors over the log. See DESIGN.md §6.1 and A.2.
//!
//! [`rebuild_from_log`]: ProjectionStore::rebuild_from_log
//! [`checkpoint`]: ProjectionStore::checkpoint
//! [`load_or_rebuild`]: ProjectionStore::load_or_rebuild

use std::marker::PhantomData;

use spork_log::Reader;

use crate::error::Result;
use crate::projection::{Checkpoint, Projection};

/// Drives a [`Projection`] against the event log: rebuild, checkpoint, restore.
///
/// The store carries no runtime state — it is a thin, type-directed dispatcher
/// over the projection seam. Construct one with [`ProjectionStore::new`] (or
/// [`Default`]) and call its methods, or call them through the type directly
/// (`ProjectionStore::<P>::rebuild_from_log(&reader)`). The `P` type parameter
/// selects which projection to build; the methods are generic over any `Reader`,
/// including one configured with migration-on-read so old-schema events are
/// upgraded before they reach [`Projection::apply`] (CLAUDE.md C5).
pub struct ProjectionStore<P: Projection> {
    _marker: PhantomData<fn() -> P>,
}

impl<P: Projection> ProjectionStore<P> {
    /// Construct a store for projection `P`.
    ///
    /// The store is stateless; this exists so call sites can hold a value and
    /// use the instance methods, but every method is equivalent to its
    /// associated-function form.
    #[must_use]
    pub fn new() -> Self {
        ProjectionStore {
            _marker: PhantomData,
        }
    }

    /// Fold the entire log into a fresh projection, from `seq 1`.
    ///
    /// Starts from [`Default`] and applies every event in ascending order. This
    /// is the canonical "drop everything and rebuild from the single source of
    /// truth" path (DESIGN.md A.6): its result depends only on the log contents,
    /// never on prior projection state.
    ///
    /// Migration-on-read, if the `reader` was configured with it, is applied
    /// transparently — [`apply`](Projection::apply) always sees payloads at
    /// their current schema version (CLAUDE.md C5).
    ///
    /// # Errors
    ///
    /// Returns [`ProjError::Log`](crate::ProjError::Log) if the log cannot be
    /// read or an event fails to decode/upgrade.
    pub fn rebuild_from_log(reader: &Reader) -> Result<P> {
        Self::replay_from(P::default(), reader, 1)
    }

    /// Capture a projection's state at `at_seq` as a canonical [`Checkpoint`].
    ///
    /// The caller asserts that `p` reflects the log through `at_seq` (i.e. every
    /// event with `seq <= at_seq` has been applied). The snapshot is canonically
    /// serialized and hashed; see [`Checkpoint::from_snapshot`].
    ///
    /// # Errors
    ///
    /// Returns [`ProjError::Canon`](crate::ProjError::Canon) if the snapshot
    /// cannot be canonically serialized.
    pub fn checkpoint(&self, p: &P, at_seq: u64) -> Result<Checkpoint> {
        Checkpoint::from_snapshot(&p.snapshot(), at_seq)
    }

    /// Restore from a checkpoint and replay the tail, or rebuild from scratch.
    ///
    /// If `ckpt` is `Some`, its hash is verified, its snapshot is decoded and
    /// restored into a projection, and the log is replayed from `at_seq + 1`
    /// onward. If `ckpt` is `None`, this is exactly [`rebuild_from_log`].
    ///
    /// The contract that makes a checkpoint safe is that
    /// `load_or_rebuild(reader, Some(ckpt))` yields a projection whose snapshot
    /// is *bit-for-bit identical* to `rebuild_from_log(reader)`, provided the
    /// checkpoint was taken from this same log — the property the crate's tests
    /// prove.
    ///
    /// # Errors
    ///
    /// - [`ProjError::CheckpointMismatch`](crate::ProjError::CheckpointMismatch)
    ///   if the checkpoint's bytes do not match its hash.
    /// - [`ProjError::Canon`](crate::ProjError::Canon) if the checkpointed
    ///   snapshot cannot be decoded.
    /// - [`ProjError::Log`](crate::ProjError::Log) if the log tail cannot be
    ///   read.
    ///
    /// [`rebuild_from_log`]: ProjectionStore::rebuild_from_log
    pub fn load_or_rebuild(reader: &Reader, ckpt: Option<Checkpoint>) -> Result<P> {
        match ckpt {
            None => Self::rebuild_from_log(reader),
            Some(ckpt) => {
                let snapshot: P::Snapshot = ckpt.decode_snapshot()?;
                let restored = P::restore(snapshot);
                // Replay strictly *after* the checkpointed sequence. `at_seq` is
                // the last applied seq, so the tail begins at `at_seq + 1`.
                // `saturating_add` is defensive against a pathological `u64::MAX`
                // checkpoint; in practice `at_seq` is a real, small sequence.
                Self::replay_from(restored, reader, ckpt.at_seq.saturating_add(1))
            }
        }
    }

    /// Apply every event with `seq >= start_seq` to `acc`, in order.
    ///
    /// Shared by [`rebuild_from_log`](ProjectionStore::rebuild_from_log) (from
    /// `1`) and [`load_or_rebuild`](ProjectionStore::load_or_rebuild) (from the
    /// checkpoint tail). Errors from the log iterator are propagated rather than
    /// skipped, so a corrupt or unreadable event aborts the fold instead of
    /// producing a silently-wrong projection.
    fn replay_from(mut acc: P, reader: &Reader, start_seq: u64) -> Result<P> {
        for item in reader.iter_from(start_seq)? {
            let event = item?;
            acc.apply(&event);
        }
        Ok(acc)
    }
}

impl<P: Projection> Default for ProjectionStore<P> {
    fn default() -> Self {
        Self::new()
    }
}
