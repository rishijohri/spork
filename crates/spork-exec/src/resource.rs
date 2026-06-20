//! Resource declarations for admission: [`ResourceProfile`], [`PooledNeed`],
//! and [`Budget`].
//!
//! Parallel branches are not free: worktrees share databases, the Docker
//! daemon, and fixed ports, causing races and flaky results, and storage
//! balloons (DESIGN.md §11.2). The scheduler therefore models scarce resources
//! as typed, capacity-bounded objects and admits sibling branches **in parallel
//! only when their resource sets are disjoint**, otherwise serializing them. A
//! [`ResourceProfile`] is each node's declaration of what it needs, split into
//! the three classes from DESIGN.md §11.2:
//!
//! - **Exclusive singletons** ([`ResourceProfile::exclusive`]) — a specific GPU,
//!   a primary DB, a license-bound service. Two profiles naming the same
//!   exclusive resource conflict.
//! - **Pooled** ([`ResourceProfile::pooled`], [`PooledNeed`]) — host ports and
//!   similar capacity-bounded pools, allocated and rewritten into env templates
//!   so most conflicts vanish automatically.
//! - **Fungible budgets** ([`ResourceProfile::fungible`], [`Budget`]) — CPU,
//!   RAM, disk.
//!
//! The chief risk is mis-declaration: under-declaring shared state causes real
//! races, so **unknown stateful services default to exclusive** — the
//! conservative choice that prevents silent cross-branch contamination
//! (DESIGN.md §11.2). That default is realized at the scheduler boundary (see
//! [`crate::scheduler`]); this module is the vocabulary it reasons over.
//!
//! Design references: DESIGN.md §5.3 (constraint-based scheduler), §11.2
//! (resource-aware scheduling: parallel vs serialized).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The current schema version stamped on a freshly constructed
/// [`ResourceProfile`].
pub const RESOURCE_PROFILE_VERSION: u16 = 1;

/// A capacity-bounded pooled resource need (e.g. host ports).
///
/// Pooled resources are allocated from a finite pool and rewritten into env
/// templates so most conflicts vanish automatically (DESIGN.md §11.2). A need
/// names the pool and how many units it requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PooledNeed {
    /// The pool this need draws from (e.g. `"ports"`).
    pub pool: String,
    /// How many units of the pool are required.
    pub count: u32,
}

impl PooledNeed {
    /// Construct a pooled need for `count` units of `pool`.
    #[must_use]
    pub fn new(pool: impl Into<String>, count: u32) -> Self {
        PooledNeed {
            pool: pool.into(),
            count,
        }
    }
}

/// A fungible budget — CPU, RAM, and disk the node expects to consume
/// (DESIGN.md §11.2).
///
/// Fungible resources never *conflict* by identity the way exclusive singletons
/// do; they are summed against a host budget. Disk in particular is the
/// motivation for CoW-by-default — many worktrees can balloon storage by several
/// multiples of repo size (DESIGN.md §11.2). Quantities are integers (no floats)
/// so a profile is canonical-hashable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Milli-CPUs requested (1000 = one core).
    pub cpu_milli: u64,
    /// Memory requested, in mebibytes.
    pub mem_mib: u64,
    /// Disk requested, in mebibytes.
    pub disk_mib: u64,
}

/// A node's declaration of the resources it needs, used by the scheduler to
/// decide parallel-vs-serial admission (DESIGN.md §11.2).
///
/// Two profiles **conflict** when they contend for the same scarce resource: a
/// shared [`exclusive`](ResourceProfile::exclusive) singleton, or a
/// [`pooled`](ResourceProfile::pooled) pool whose combined demand the scheduler
/// cannot satisfy at once. The scheduler is the authority on conflict; this type
/// provides [`exclusive_overlap`](ResourceProfile::exclusive_overlap) as the
/// primitive it builds on. The struct carries its own
/// [`schema_version`](ResourceProfile::schema_version) (constraint C5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceProfile {
    /// The schema version of this profile ([`RESOURCE_PROFILE_VERSION`] for
    /// fresh values).
    pub schema_version: u16,
    /// Exclusive singletons this node requires by name (e.g.
    /// `"db:postgres-primary"`, `"gpu:0"`). Any two nodes naming the same
    /// exclusive resource conflict.
    pub exclusive: Vec<String>,
    /// Pooled, capacity-bounded needs (e.g. host ports).
    pub pooled: Vec<PooledNeed>,
    /// Fungible CPU/RAM/disk budget.
    pub fungible: Budget,
}

impl Default for ResourceProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceProfile {
    /// Construct an empty profile (needs nothing exclusive or pooled, zero
    /// budget), stamped with the current [`RESOURCE_PROFILE_VERSION`].
    ///
    /// An empty profile conflicts with nothing — it is the profile of a node
    /// that touches no scarce resource and can always run in parallel.
    #[must_use]
    pub fn new() -> Self {
        ResourceProfile {
            schema_version: RESOURCE_PROFILE_VERSION,
            exclusive: Vec::new(),
            pooled: Vec::new(),
            fungible: Budget::default(),
        }
    }

    /// Add an exclusive singleton requirement (builder style).
    #[must_use]
    pub fn with_exclusive(mut self, resource: impl Into<String>) -> Self {
        self.exclusive.push(resource.into());
        self
    }

    /// Add a pooled need (builder style).
    #[must_use]
    pub fn with_pooled(mut self, need: PooledNeed) -> Self {
        self.pooled.push(need);
        self
    }

    /// Set the fungible budget (builder style).
    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.fungible = budget;
        self
    }

    /// The set of exclusive resources this profile names, de-duplicated.
    #[must_use]
    pub fn exclusive_set(&self) -> BTreeSet<&str> {
        self.exclusive.iter().map(String::as_str).collect()
    }

    /// The exclusive singletons this profile shares with `other`.
    ///
    /// A non-empty overlap is the primitive conflict signal: two nodes that both
    /// require the same exclusive resource cannot run in parallel (DESIGN.md
    /// §11.2). Returns the shared resource names, sorted, so a conflict can be
    /// reported with a precise reason.
    #[must_use]
    pub fn exclusive_overlap(&self, other: &ResourceProfile) -> Vec<String> {
        let mine = self.exclusive_set();
        let theirs = other.exclusive_set();
        mine.intersection(&theirs)
            .map(|s| (*s).to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty_and_versioned() {
        let p = ResourceProfile::new();
        assert_eq!(p.schema_version, RESOURCE_PROFILE_VERSION);
        assert!(p.exclusive.is_empty());
        assert!(p.pooled.is_empty());
        assert_eq!(p.fungible, Budget::default());
        assert_eq!(ResourceProfile::default(), p);
    }

    #[test]
    fn builder_assembles_a_profile() {
        let p = ResourceProfile::new()
            .with_exclusive("db:postgres-primary")
            .with_pooled(PooledNeed::new("ports", 2))
            .with_budget(Budget {
                cpu_milli: 2000,
                mem_mib: 4096,
                disk_mib: 8192,
            });
        assert_eq!(p.exclusive, vec!["db:postgres-primary".to_string()]);
        assert_eq!(p.pooled, vec![PooledNeed::new("ports", 2)]);
        assert_eq!(p.fungible.cpu_milli, 2000);
    }

    #[test]
    fn empty_profiles_never_overlap() {
        let a = ResourceProfile::new();
        let b = ResourceProfile::new();
        assert!(a.exclusive_overlap(&b).is_empty());
    }

    #[test]
    fn shared_exclusive_resource_overlaps() {
        let a = ResourceProfile::new().with_exclusive("db:postgres-primary");
        let b = ResourceProfile::new()
            .with_exclusive("db:postgres-primary")
            .with_exclusive("gpu:0");
        assert_eq!(
            a.exclusive_overlap(&b),
            vec!["db:postgres-primary".to_string()]
        );
    }

    #[test]
    fn disjoint_exclusive_resources_do_not_overlap() {
        let a = ResourceProfile::new().with_exclusive("gpu:0");
        let b = ResourceProfile::new().with_exclusive("gpu:1");
        assert!(a.exclusive_overlap(&b).is_empty());
    }

    #[test]
    fn profile_round_trips_through_serde() {
        let p = ResourceProfile::new()
            .with_exclusive("db:postgres-primary")
            .with_pooled(PooledNeed::new("ports", 3));
        let json = serde_json::to_string(&p).unwrap();
        let back: ResourceProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
