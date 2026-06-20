//! The projection seam: the [`Projection`] trait and the [`Checkpoint`] type.
//!
//! A *projection* is a pure, deterministic fold over the event log. It owns no
//! truth of its own — the log is the single source of truth (DESIGN.md §6.1,
//! "The two layers") — so a projection can always be dropped and rebuilt
//! bit-for-bit by replaying events, which is the soundness guarantee in
//! DESIGN.md A.6 ("Self-Testing, Crash Recovery & Observability"). To make
//! rebuilds cheap, a projection can emit a [`Checkpoint`]: the
//! canonically-serialized snapshot of its state at a known `seq`, plus the
//! BLAKE3 hash of those bytes. Restoring from a checkpoint and replaying only
//! the events after it must produce *exactly* the same state — and the same
//! snapshot hash — as a full replay (the bit-for-bit identity this crate
//! proves).

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use spork_log::Event;

use crate::error::{ProjError, Result};

/// A pure, rebuildable fold over the event log.
///
/// Implementors describe how to evolve in-memory state one [`Event`] at a time
/// ([`apply`]), how to capture that state as a serializable snapshot
/// ([`snapshot`]), and how to reconstruct the state from such a snapshot
/// ([`restore`]). These three methods are the whole seam (CLAUDE.md C3): the
/// generic [`ProjectionStore`] drives rebuild, checkpoint, and load-or-rebuild
/// entirely through them, so a new projection is a new `impl` and nothing more.
///
/// # Laws
///
/// An implementation must obey three laws for the store's guarantees to hold:
///
/// 1. **Determinism.** `apply` is a pure function of the current state and the
///    event; replaying the same event sequence from [`Default`] always yields
///    the same state. No clocks, randomness, or ambient I/O.
/// 2. **Snapshot round-trip.** `restore(p.snapshot())` is observationally equal
///    to `p` (its `snapshot()` equals `p.snapshot()`).
/// 3. **Canonical stability.** `Snapshot` serializes to *canonical* bytes
///    deterministically (see `spork-canon`), so equal states hash equally on
///    any machine and across runs. In practice this means using ordered
///    containers (e.g. [`BTreeMap`]) rather than hash-ordered ones in the
///    snapshot, and integers rather than floats.
///
/// Under these laws, a full replay and a checkpoint-plus-replay produce an
/// identical snapshot hash, and dropping the projection and rebuilding from the
/// log reproduces it exactly (DESIGN.md A.6).
///
/// [`apply`]: Projection::apply
/// [`snapshot`]: Projection::snapshot
/// [`restore`]: Projection::restore
/// [`ProjectionStore`]: crate::ProjectionStore
/// [`BTreeMap`]: std::collections::BTreeMap
pub trait Projection: Default {
    /// The serializable, comparable snapshot of this projection's state.
    ///
    /// It must canonically serialize deterministically (law 3 above) so that
    /// checkpoints are byte-stable, and be `PartialEq` so tests and the store
    /// can assert that two reconstructions agree.
    type Snapshot: Serialize + DeserializeOwned + PartialEq;

    /// Fold a single event into the projection's state.
    ///
    /// Called once per event, in ascending `seq` order, with no gaps. Must be a
    /// pure function of the current state and `event` (law 1).
    fn apply(&mut self, event: &Event);

    /// Capture the current state as a snapshot.
    ///
    /// The snapshot is what gets canonically serialized into a [`Checkpoint`];
    /// it must fully determine the state so that [`restore`] can rebuild it.
    ///
    /// [`restore`]: Projection::restore
    fn snapshot(&self) -> Self::Snapshot;

    /// Reconstruct a projection from a previously captured snapshot.
    ///
    /// The inverse of [`snapshot`]: `restore(p.snapshot())` must be
    /// observationally equal to `p` (law 2). This is how a [`Checkpoint`] is
    /// turned back into live state before replaying the tail of the log.
    ///
    /// [`snapshot`]: Projection::snapshot
    fn restore(snapshot: Self::Snapshot) -> Self;
}

/// A canonical, hash-pinned snapshot of a projection at a known sequence.
///
/// A checkpoint records the projection's state as of `at_seq` (meaning: after
/// folding every event with `seq <= at_seq`). It stores the **canonical bytes**
/// of the snapshot (via `spork-canon`, the same encoder the whole substrate
/// hashes through) together with their BLAKE3 `snapshot_hash`. Storing the
/// canonical bytes — not a re-serialization at load time — is what makes the
/// hash a stable identity: two checkpoints of the same logical state are
/// byte-identical and hash-identical, on any machine and across runs.
///
/// A checkpoint is an *optimization*, never a source of truth: it can always be
/// discarded and the projection rebuilt from the log. [`verify`] re-derives the
/// hash from the bytes so a tampered or corrupted checkpoint is detected rather
/// than trusted (DESIGN.md A.2, A.6).
///
/// The struct is itself `Serialize`/`Deserialize` so callers can persist it
/// (alongside the log) however they choose; per CLAUDE.md C5 it carries an
/// explicit [`schema_version`](Checkpoint::schema_version) so the on-disk form
/// can evolve additively through a migration registry rather than in place.
///
/// [`verify`]: Checkpoint::verify
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The schema version of this checkpoint envelope (CLAUDE.md C5).
    ///
    /// Frozen at [`Checkpoint::SCHEMA_VERSION`] for the F1 generation. A future
    /// change to the checkpoint envelope's shape is a version bump plus a
    /// registered forward-migration, never an in-place edit of stored bytes.
    pub schema_version: u16,
    /// The sequence number through which this snapshot folds the log: state is
    /// as of every event with `seq <= at_seq`. `0` means "before any event"
    /// (the [`Default`] projection state).
    pub at_seq: u64,
    /// The canonical (`spork-canon`) serialized bytes of the snapshot.
    pub snapshot_bytes: Vec<u8>,
    /// The BLAKE3 hash of `snapshot_bytes` — the checkpoint's stable identity.
    pub snapshot_hash: Hash,
}

impl Checkpoint {
    /// The frozen schema version of the [`Checkpoint`] envelope (CLAUDE.md C5).
    pub const SCHEMA_VERSION: u16 = 1;

    /// Build a checkpoint from a projection's snapshot and the sequence it
    /// covers.
    ///
    /// The snapshot is canonically serialized (`spork-canon`) and the resulting
    /// bytes are hashed (`spork-hash`); both are stored so the hash is a stable
    /// identity that never depends on a later re-serialization.
    ///
    /// # Errors
    ///
    /// Returns [`ProjError::Canon`] if the snapshot cannot be canonically
    /// serialized (for example it contains a float, which is forbidden in
    /// identity-bearing data — see `spork-canon`).
    pub fn from_snapshot<S: Serialize>(snapshot: &S, at_seq: u64) -> Result<Self> {
        let snapshot_bytes = spork_canon::canonicalize(snapshot)?;
        let snapshot_hash = spork_hash::hash_bytes(&snapshot_bytes);
        Ok(Checkpoint {
            schema_version: Self::SCHEMA_VERSION,
            at_seq,
            snapshot_bytes,
            snapshot_hash,
        })
    }

    /// Re-derive the hash from the bytes and check it matches `snapshot_hash`.
    ///
    /// This is the integrity check for a loaded checkpoint: if the stored bytes
    /// were altered after the hash was recorded, the recomputation differs and
    /// the checkpoint is rejected. A projection is always rebuildable from the
    /// log, so a failing checkpoint is recoverable — but it is never silently
    /// trusted.
    ///
    /// # Errors
    ///
    /// Returns [`ProjError::CheckpointMismatch`] if the recomputed hash differs
    /// from the recorded one.
    pub fn verify(&self) -> Result<()> {
        if spork_hash::hash_bytes(&self.snapshot_bytes) != self.snapshot_hash {
            return Err(ProjError::CheckpointMismatch);
        }
        Ok(())
    }

    /// Decode the snapshot stored in this checkpoint, after verifying its hash.
    ///
    /// The bytes are canonical JSON (a strict subset of JSON; see `spork-canon`)
    /// and re-parse to the snapshot value. The hash is verified first so a
    /// corrupted checkpoint never decodes into a plausible-looking but wrong
    /// state.
    ///
    /// # Errors
    ///
    /// - [`ProjError::CheckpointMismatch`] if [`verify`](Checkpoint::verify)
    ///   fails.
    /// - [`ProjError::Canon`] if the bytes do not decode into `S` (surfaced
    ///   through the canon error channel since they are canonical-JSON bytes).
    pub fn decode_snapshot<S: DeserializeOwned>(&self) -> Result<S> {
        self.verify()?;
        serde_json::from_slice(&self.snapshot_bytes)
            .map_err(|e| ProjError::Canon(format!("failed to decode snapshot: {e}")))
    }
}
