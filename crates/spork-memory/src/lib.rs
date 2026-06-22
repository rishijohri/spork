//! Spork P7 memory store (DESIGN.md §13.6).
//!
//! The memory store is three-tier — **project** (stable facts), **lineage**
//! (carried down a branch), and **node** (ephemeral notes) — with provenance, a
//! TTL, usage decay, and pinning to fight staleness and poisoning. Recall ranks
//! by [`Embedder`] similarity; per DESIGN.md §13.6 vector similarity is used
//! **only for memory recall, never for code retrieval** (that is the repo map's
//! job, `spork-repomap`). The embedder is a seam with a dependency-free lexical
//! v1; sibling-branch lineage is excluded from scoped recall by construction
//! (Open Question 4 — sibling context default isolated).
//!
//! # Example
//!
//! ```
//! use spork_memory::{MemoryStore, MemoryEntry, MemoryScope, LexicalEmbedder};
//! use ulid::Ulid;
//!
//! let mut store = MemoryStore::new();
//! store.put(MemoryEntry::new(Ulid::new(), MemoryScope::Project,
//!     "use exponential backoff for retries", "user", 0));
//! let hits = store.recall(&LexicalEmbedder::new(), "how should retries back off", 10, 5);
//! assert!(hits[0].text.contains("backoff"));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod embed;
mod store;

pub use embed::{Embedder, Embedding, LexicalEmbedder};
pub use store::{MemoryEntry, MemoryScope, MemoryStore, MEMORY_ENTRY_SCHEMA_VERSION};
