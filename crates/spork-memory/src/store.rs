//! The three-tier [`MemoryStore`] with provenance, TTL, usage decay, and pinning
//! (DESIGN.md §13.6).
//!
//! The memory store is three-tier — **project** (stable facts), **lineage**
//! (carried down a branch), and **node** (ephemeral notes) — with provenance, a
//! TTL, usage decay, and pinning to fight staleness and poisoning. Recall ranks
//! entries by [`Embedder`] similarity (vector similarity is for memory recall
//! only), with pinned and frequently-used entries boosted, and never returns an
//! expired entry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::embed::Embedder;

/// The frozen schema version of the [`MemoryEntry`] shape (CLAUDE.md C5).
pub const MEMORY_ENTRY_SCHEMA_VERSION: u16 = 1;

/// Which tier a memory entry lives in (DESIGN.md §13.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// Project-wide stable facts (always in scope).
    Project,
    /// Facts carried down a specific branch lineage.
    Lineage,
    /// Ephemeral notes attached to a single node.
    Node,
}

/// One memory entry (DESIGN.md §13.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// The schema version of this entry (CLAUDE.md C5).
    pub schema_version: u16,
    /// A unique, time-sortable id.
    pub id: Ulid,
    /// Which tier this entry lives in.
    pub scope: MemoryScope,
    /// The scope binding: a project key, a lineage hash (hex), or a node id —
    /// `None` for [`MemoryScope::Project`] (it is global).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_ref: Option<String>,
    /// The remembered text.
    pub text: String,
    /// Where this memory came from (the §13.6 provenance — e.g. a node id, "user").
    pub provenance: String,
    /// Creation time in epoch milliseconds.
    pub created_ms: u64,
    /// Last-used time in epoch milliseconds (updated on recall via
    /// [`MemoryStore::touch`]).
    pub last_used_ms: u64,
    /// Optional time-to-live in milliseconds; the entry expires at
    /// `created_ms + ttl_ms` unless pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// Whether the entry is pinned (protected from TTL expiry and decay eviction).
    pub pinned: bool,
    /// How many times this entry has been recalled (for usage-decay ranking).
    pub usage_count: u64,
}

impl MemoryEntry {
    /// Construct an entry at `created_ms`, unpinned, with no TTL.
    #[must_use]
    pub fn new(
        id: Ulid,
        scope: MemoryScope,
        text: impl Into<String>,
        provenance: impl Into<String>,
        created_ms: u64,
    ) -> Self {
        MemoryEntry {
            schema_version: MEMORY_ENTRY_SCHEMA_VERSION,
            id,
            scope,
            scope_ref: None,
            text: text.into(),
            provenance: provenance.into(),
            created_ms,
            last_used_ms: created_ms,
            ttl_ms: None,
            pinned: false,
            usage_count: 0,
        }
    }

    /// Bind the entry to a scope ref (a lineage hash or node id) (builder style).
    #[must_use]
    pub fn with_scope_ref(mut self, scope_ref: impl Into<String>) -> Self {
        self.scope_ref = Some(scope_ref.into());
        self
    }

    /// Set a TTL in milliseconds (builder style).
    #[must_use]
    pub fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = Some(ttl_ms);
        self
    }

    /// Pin the entry (builder style).
    #[must_use]
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// Whether the entry is expired at `now_ms` (pinned entries never expire).
    #[must_use]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        if self.pinned {
            return false;
        }
        match self.ttl_ms {
            Some(ttl) => self.created_ms.saturating_add(ttl) <= now_ms,
            None => false,
        }
    }
}

/// The three-tier memory store (DESIGN.md §13.6).
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    entries: BTreeMap<Ulid, MemoryEntry>,
}

impl MemoryStore {
    /// Construct an empty memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) an entry, returning its id.
    pub fn put(&mut self, entry: MemoryEntry) -> Ulid {
        let id = entry.id;
        self.entries.insert(id, entry);
        id
    }

    /// Fetch an entry by id.
    #[must_use]
    pub fn get(&self, id: Ulid) -> Option<&MemoryEntry> {
        self.entries.get(&id)
    }

    /// Record a use of an entry (bumps `usage_count` and `last_used_ms`).
    pub fn touch(&mut self, id: Ulid, now_ms: u64) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.usage_count = e.usage_count.saturating_add(1);
            e.last_used_ms = now_ms;
        }
    }

    /// The number of stored entries (including expired-but-not-yet-GC'd).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove expired, non-pinned entries, returning their ids (DESIGN.md §13.6
    /// TTL).
    pub fn gc(&mut self, now_ms: u64) -> Vec<Ulid> {
        let expired: Vec<Ulid> = self
            .entries
            .values()
            .filter(|e| e.is_expired(now_ms))
            .map(|e| e.id)
            .collect();
        for id in &expired {
            self.entries.remove(id);
        }
        expired
    }

    /// Evict non-pinned entries unused for longer than `max_idle_ms` (usage
    /// decay — DESIGN.md §13.6), returning their ids.
    pub fn prune_stale(&mut self, now_ms: u64, max_idle_ms: u64) -> Vec<Ulid> {
        let stale: Vec<Ulid> = self
            .entries
            .values()
            .filter(|e| !e.pinned && now_ms.saturating_sub(e.last_used_ms) > max_idle_ms)
            .map(|e| e.id)
            .collect();
        for id in &stale {
            self.entries.remove(id);
        }
        stale
    }

    /// Recall the most relevant non-expired entries for `query` over all scopes.
    ///
    /// Ranks by [`Embedder`] cosine similarity, with pinned and frequently-used
    /// entries boosted; never returns an expired entry. Read-only — call
    /// [`touch`](MemoryStore::touch) to record a recall.
    #[must_use]
    pub fn recall(
        &self,
        embedder: &dyn Embedder,
        query: &str,
        now_ms: u64,
        top_k: usize,
    ) -> Vec<MemoryEntry> {
        self.recall_filtered(embedder, query, now_ms, top_k, |_| true)
    }

    /// Recall scoped to a node's context: project memory (always), the given
    /// lineage, and the given node (DESIGN.md §13.6).
    ///
    /// This is the compiler's recall path — sibling-branch lineage is excluded by
    /// construction (a different `lineage_ref` never matches), honoring the
    /// sibling-isolation default (Open Question 4).
    #[must_use]
    pub fn recall_scoped(
        &self,
        embedder: &dyn Embedder,
        query: &str,
        lineage_ref: &str,
        node_ref: &str,
        now_ms: u64,
        top_k: usize,
    ) -> Vec<MemoryEntry> {
        self.recall_filtered(embedder, query, now_ms, top_k, |e| match e.scope {
            MemoryScope::Project => true,
            MemoryScope::Lineage => e.scope_ref.as_deref() == Some(lineage_ref),
            MemoryScope::Node => e.scope_ref.as_deref() == Some(node_ref),
        })
    }

    fn recall_filtered(
        &self,
        embedder: &dyn Embedder,
        query: &str,
        now_ms: u64,
        top_k: usize,
        in_scope: impl Fn(&MemoryEntry) -> bool,
    ) -> Vec<MemoryEntry> {
        let q = embedder.embed(query);
        let mut scored: Vec<(u64, MemoryEntry)> = self
            .entries
            .values()
            .filter(|e| !e.is_expired(now_ms) && in_scope(e))
            .map(|e| {
                let sim = u64::from(embedder.embed(&e.text).cosine_bps(&q));
                // Pinned entries get a flat boost; usage adds a small lift. The
                // similarity dominates so recall stays query-relevant.
                let pin_boost = if e.pinned { 500 } else { 0 };
                let score = sim + pin_boost + e.usage_count.min(50);
                (score, e.clone())
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        // Sort by score desc, then most-recent first, then id for stability.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.last_used_ms.cmp(&a.1.last_used_ms))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        scored.into_iter().take(top_k).map(|(_, e)| e).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::LexicalEmbedder;

    fn entry(id: Ulid, scope: MemoryScope, text: &str, created: u64) -> MemoryEntry {
        MemoryEntry::new(id, scope, text, "test", created)
    }

    #[test]
    fn schema_version_frozen() {
        assert_eq!(MEMORY_ENTRY_SCHEMA_VERSION, 1);
    }

    #[test]
    fn recall_ranks_by_similarity() {
        let mut store = MemoryStore::new();
        store.put(entry(
            Ulid::new(),
            MemoryScope::Project,
            "use exponential backoff for retries",
            0,
        ));
        store.put(entry(
            Ulid::new(),
            MemoryScope::Project,
            "the UI uses a dark theme",
            0,
        ));
        let hits = store.recall(
            &LexicalEmbedder::new(),
            "how should retries back off",
            10,
            5,
        );
        assert!(!hits.is_empty());
        assert!(hits[0].text.contains("backoff"));
    }

    #[test]
    fn expired_entries_are_not_recalled() {
        let mut store = MemoryStore::new();
        let id = Ulid::new();
        store.put(entry(id, MemoryScope::Node, "ephemeral retry note", 0).with_ttl_ms(100));
        // Before expiry: recalled.
        assert!(!store
            .recall(&LexicalEmbedder::new(), "retry note", 50, 5)
            .is_empty());
        // After expiry: gone.
        assert!(store
            .recall(&LexicalEmbedder::new(), "retry note", 200, 5)
            .is_empty());
    }

    #[test]
    fn pinned_entry_never_expires() {
        let mut store = MemoryStore::new();
        store.put(
            entry(Ulid::new(), MemoryScope::Project, "pinned retry fact", 0)
                .with_ttl_ms(100)
                .pinned(true),
        );
        assert!(!store
            .recall(&LexicalEmbedder::new(), "retry fact", 10_000, 5)
            .is_empty());
        // GC also leaves it alone.
        assert!(store.gc(10_000).is_empty());
    }

    #[test]
    fn gc_removes_expired_unpinned() {
        let mut store = MemoryStore::new();
        let id = store.put(entry(Ulid::new(), MemoryScope::Node, "temp", 0).with_ttl_ms(10));
        let removed = store.gc(100);
        assert_eq!(removed, vec![id]);
        assert!(store.is_empty());
    }

    #[test]
    fn prune_stale_evicts_idle_unpinned() {
        let mut store = MemoryStore::new();
        let idle = store.put(entry(Ulid::new(), MemoryScope::Node, "idle note", 0));
        let pinned =
            store.put(entry(Ulid::new(), MemoryScope::Node, "pinned note", 0).pinned(true));
        // now=10_000, max_idle=1_000 -> idle (last_used 0) evicted, pinned kept.
        let pruned = store.prune_stale(10_000, 1_000);
        assert_eq!(pruned, vec![idle]);
        assert!(store.get(pinned).is_some());
    }

    #[test]
    fn recall_scoped_excludes_sibling_lineage() {
        let mut store = MemoryStore::new();
        store.put(
            entry(
                Ulid::new(),
                MemoryScope::Lineage,
                "branch A retry decision",
                0,
            )
            .with_scope_ref("lineage-A"),
        );
        store.put(
            entry(
                Ulid::new(),
                MemoryScope::Lineage,
                "branch B retry decision",
                0,
            )
            .with_scope_ref("lineage-B"),
        );
        store.put(entry(
            Ulid::new(),
            MemoryScope::Project,
            "global retry policy",
            0,
        ));
        let hits = store.recall_scoped(
            &LexicalEmbedder::new(),
            "retry decision",
            "lineage-A",
            "node-1",
            10,
            10,
        );
        // Project + lineage-A only; lineage-B (sibling) excluded.
        assert!(hits.iter().any(|e| e.text.contains("branch A")));
        assert!(hits.iter().any(|e| e.text.contains("global")));
        assert!(!hits.iter().any(|e| e.text.contains("branch B")));
    }

    #[test]
    fn touch_increments_usage_and_recency() {
        let mut store = MemoryStore::new();
        let id = store.put(entry(Ulid::new(), MemoryScope::Project, "fact", 0));
        store.touch(id, 500);
        let e = store.get(id).unwrap();
        assert_eq!(e.usage_count, 1);
        assert_eq!(e.last_used_ms, 500);
    }

    #[test]
    fn entry_round_trips_and_canonicalizes() {
        let e = entry(Ulid::new(), MemoryScope::Lineage, "fact", 0)
            .with_scope_ref("lin")
            .with_ttl_ms(1000)
            .pinned(true);
        let v = serde_json::to_value(&e).unwrap();
        let back: MemoryEntry = serde_json::from_value(v).unwrap();
        assert_eq!(e, back);
        assert!(spork_canon::canonicalize(&e).is_ok());
    }
}
