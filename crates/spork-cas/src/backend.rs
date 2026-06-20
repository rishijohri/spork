//! The frozen storage seam: [`StorageBackend`].
//!
//! This trait is the *one place* the rest of Spork talks to physical object
//! storage. It is deliberately tiny — store bytes under a kind/tag, fetch bytes
//! by digest, test for presence — so that new backends (a packfile store, a
//! remote/object-storage backend, an in-memory test double) are added behind the
//! same interface without touching any caller (constraint C3). v1 ships exactly
//! one production backend, [`crate::LooseStore`] (loose objects + a real
//! packfile read-through); this trait is what makes the others additive.
//!
//! # Contract
//!
//! - [`StorageBackend::put`] writes `bytes` (the *payload*; the implementation
//!   frames it with the self-describing header) under the digest of those bytes
//!   and returns that digest. It is idempotent: storing identical bytes twice
//!   leaves a single object and returns the same digest.
//! - [`StorageBackend::get`] returns the *payload* bytes for a digest, or `None`
//!   if absent. It validates the stored header and **rejects an unknown
//!   generation** ([`CasError::UnknownGeneration`]) rather than returning bytes
//!   from a scheme it does not understand.
//! - [`StorageBackend::has`] reports presence without materializing the payload.
//!
//! Design references: DESIGN.md §6.1 (content identity), §10.1 (object model /
//! loose objects + packfiles), Appendix A.7 C-3 (the no-domino generation seam).

use spork_hash::{Hash, HashTag};

use crate::error::Result;
use crate::object::ObjKind;

/// The physical object-storage seam.
///
/// Implementations persist opaque object payloads keyed by the BLAKE3 digest of
/// the payload bytes, wrapping each with the self-describing header
/// ([`crate::header`]) so reads can reject unknown hash generations.
///
/// See the [module docs](crate::backend) for the full contract. The trait is
/// frozen for v1; capabilities are extended by adding new implementations, never
/// by editing this interface.
pub trait StorageBackend {
    /// Store `bytes` (an object *payload*) under `tag` and `kind`, returning the
    /// digest the object is addressed by.
    ///
    /// The returned digest is `BLAKE3(bytes)` (the payload's digest), which is
    /// the object's content address. Storing identical bytes again is a no-op
    /// that returns the same digest (idempotent / dedup by hash).
    ///
    /// # Errors
    /// Returns a [`CasError`](crate::CasError) on I/O failure or if the bytes
    /// cannot be framed/written.
    fn put(&self, tag: HashTag, kind: ObjKind, bytes: &[u8]) -> Result<Hash>;

    /// Fetch the payload bytes for `hash`, or `None` if the object is absent.
    ///
    /// Validates the stored object's header and verifies that the payload hashes
    /// to `hash` (integrity).
    ///
    /// # Errors
    /// - [`CasError::UnknownGeneration`](crate::CasError::UnknownGeneration) /
    ///   [`CasError::UnknownAlgo`](crate::CasError::UnknownAlgo) — the stored
    ///   header names a hash scheme this build does not support (no-domino
    ///   rejection).
    /// - [`CasError::MalformedHeader`](crate::CasError::MalformedHeader) —
    ///   corrupt header.
    /// - [`CasError::IntegrityMismatch`](crate::CasError::IntegrityMismatch) —
    ///   the bytes do not hash to the digest they were stored under.
    /// - [`CasError::Io`](crate::CasError::Io) — read failure.
    fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>>;

    /// Report whether an object with `hash` exists in the store.
    ///
    /// # Errors
    /// Returns a [`CasError`](crate::CasError) on I/O failure.
    fn has(&self, hash: &Hash) -> Result<bool>;

    /// Durably store a *batch* of new objects under a **single durability
    /// barrier**, returning their digests in input order.
    ///
    /// Each element is `(tag, kind, payload)`; the digest of element `i` is
    /// `BLAKE3(payload_i)`, exactly as [`StorageBackend::put`] would return. The
    /// contract is identical to calling [`StorageBackend::put`] on every element
    /// *except* for the durability shape:
    ///
    /// - **Single fsync.** A bulk capture ([`crate::ObjectStore::put_tree`] /
    ///   [`crate::ObjectStore::put_snapshot`]) produces many new objects; folding
    ///   them into one batch lets a backend pay *one* fsync for the whole set
    ///   instead of one per object — the dominant cost of a cold capture.
    /// - **All-or-nothing durability.** When this method returns `Ok`, **every**
    ///   returned digest is durable on disk. This preserves the
    ///   write-objects-then-log ordering invariant F1 depends on (DESIGN.md
    ///   §10.4): a caller may safely record the batch in the op-log only after
    ///   this returns.
    /// - **Crash safety.** Because objects are content-addressed and immutable, a
    ///   crash *before* this returns may leave nothing, a partial set, or
    ///   everything — never a corrupt or dangling reference. A discarded partial
    ///   batch is simply missing objects, re-created idempotently on retry, or
    ///   reclaimable orphans (DESIGN.md §10.4 crash-consistency row).
    ///
    /// Idempotent like [`StorageBackend::put`]: a payload already present is
    /// reused, and its digest is still returned. The default implementation
    /// simply calls [`StorageBackend::put`] for each element, so a backend that
    /// has no cheaper batch path needs no override; [`crate::LooseStore`]
    /// overrides it to write the new objects into a single packfile, fsynced
    /// once.
    ///
    /// # Errors
    /// Returns a [`CasError`](crate::CasError) on I/O failure or if any element
    /// cannot be framed/written.
    fn put_batch(&self, objects: &[(HashTag, ObjKind, Vec<u8>)]) -> Result<Vec<Hash>> {
        objects
            .iter()
            .map(|(tag, kind, bytes)| self.put(*tag, *kind, bytes))
            .collect()
    }
}
