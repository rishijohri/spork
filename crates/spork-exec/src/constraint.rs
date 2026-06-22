//! The P7 [`ConstraintScheduler`]: full-strength resource-aware admission
//! (DESIGN.md §11.2).
//!
//! F4 shipped the conservative [`SerialScheduler`](crate::SerialScheduler) — one
//! workspace at a time — behind the frozen [`Scheduler`](crate::Scheduler) trait.
//! P7 adds this richer implementation **behind that same trait** (CLAUDE.md C2/C3,
//! PLAN §9 D-9): siblings whose resource sets are disjoint run in **parallel**;
//! the rest are **serialized with a precise reason** ("conflict on
//! `gpu:0`", "pool `ports` exhausted", "insufficient mem"). It also models the
//! two §11.2 mitigations that make most conflicts vanish:
//!
//! - **Pooled port allocation + env rewrite** — ports are handed out from a
//!   finite pool and written into per-candidate env vars, so two siblings get
//!   distinct ports instead of colliding on a fixed one.
//! - **Per-branch ephemeral DB provisioning** — an exclusive marked as an
//!   *ephemeral DB pool* (e.g. `db:postgres-primary`, via
//!   `CREATE DATABASE … TEMPLATE`) is rewritten to a per-candidate database name,
//!   so two siblings that both "need the primary DB" no longer serialize.
//!
//! The richer per-candidate allocation (ports, DBs, env) is exposed additively on
//! the struct via [`ConstraintScheduler::plan`]; the trait method
//! [`Scheduler::admit`] returns only the parallel-vs-serial
//! [`AdmissionPlan`](crate::AdmissionPlan), so callers of the frozen seam are
//! unchanged. **Unknown stateful services still default to exclusive** (DESIGN.md
//! §11.2): an exclusive that is not registered as an ephemeral-DB pool conflicts.

use std::collections::{BTreeMap, BTreeSet};

use crate::resource::Budget;
use crate::scheduler::{Admission, AdmissionPlan, Disposition, ScheduledItem, Scheduler};

/// A finite pool of consecutively-numbered ports (DESIGN.md §11.2 "Pooled").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortPool {
    /// The first port number in the pool.
    pub base: u32,
    /// How many ports the pool holds (`base..base+capacity`).
    pub capacity: u32,
}

impl PortPool {
    /// Construct a port pool over `base..base+capacity`.
    #[must_use]
    pub fn new(base: u32, capacity: u32) -> Self {
        PortPool { base, capacity }
    }
}

/// One admitted candidate's concrete resource allocation (DESIGN.md §11.2).
///
/// `ports` maps each pool to the port numbers handed to this candidate;
/// `ephemeral_dbs` maps each ephemeral-DB exclusive to the per-candidate database
/// name provisioned for it; `env` is the merged set of environment-variable
/// rewrites (port and DB bindings) the executor applies so the candidate uses its
/// allocated resources instead of fixed ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// The candidate id.
    pub id: String,
    /// Allocated ports per pool.
    pub ports: BTreeMap<String, Vec<u32>>,
    /// Per-branch ephemeral database names, keyed by the exclusive they replace.
    pub ephemeral_dbs: BTreeMap<String, String>,
    /// Environment-variable rewrites (port + DB bindings).
    pub env: BTreeMap<String, String>,
}

/// The full constraint plan: the [`AdmissionPlan`] plus per-admitted allocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintPlan {
    /// The parallel-vs-serial admission decision (the frozen-trait return).
    pub admission: AdmissionPlan,
    /// One allocation per admitted candidate, in admission order.
    pub allocations: Vec<Allocation>,
}

/// The P7 full-strength constraint scheduler (DESIGN.md §11.2).
#[derive(Debug, Clone, Default)]
pub struct ConstraintScheduler {
    pools: BTreeMap<String, PortPool>,
    host_budget: Budget,
    ephemeral_dbs: BTreeSet<String>,
}

impl ConstraintScheduler {
    /// Construct a scheduler with no pools, an unbounded budget, and no ephemeral
    /// DB pools (so it behaves like a pure disjoint-exclusive parallelizer).
    #[must_use]
    pub fn new() -> Self {
        ConstraintScheduler {
            pools: BTreeMap::new(),
            host_budget: Budget {
                cpu_milli: u64::MAX,
                mem_mib: u64::MAX,
                disk_mib: u64::MAX,
            },
            ephemeral_dbs: BTreeSet::new(),
        }
    }

    /// Register a pooled resource (e.g. host ports) (builder style).
    #[must_use]
    pub fn with_pool(mut self, name: impl Into<String>, pool: PortPool) -> Self {
        self.pools.insert(name.into(), pool);
        self
    }

    /// Set the total host budget the sum of admitted candidates must fit within
    /// (builder style).
    #[must_use]
    pub fn with_host_budget(mut self, budget: Budget) -> Self {
        self.host_budget = budget;
        self
    }

    /// Register an exclusive resource as a **per-branch ephemeral DB pool**, so
    /// candidates that declare it get their own database instead of serializing
    /// (DESIGN.md §11.2) (builder style).
    #[must_use]
    pub fn with_ephemeral_db(mut self, exclusive: impl Into<String>) -> Self {
        self.ephemeral_dbs.insert(exclusive.into());
        self
    }

    /// Compute the full constraint plan (admission + allocations).
    ///
    /// The first candidate is always admitted (something must run, mirroring the
    /// serial scheduler's invariant); each subsequent candidate is admitted iff
    /// its hard-exclusive set is disjoint from the admitted set, its pooled needs
    /// fit the remaining pool capacity, and its budget fits the remaining host
    /// budget — otherwise it is serialized with a precise reason.
    #[must_use]
    pub fn plan(&self, candidates: Vec<Admission>) -> ConstraintPlan {
        let mut items: Vec<ScheduledItem> = Vec::with_capacity(candidates.len());
        let mut allocations: Vec<Allocation> = Vec::new();

        // Running state across admitted candidates.
        let mut owner_of_exclusive: BTreeMap<String, String> = BTreeMap::new();
        let mut pool_used: BTreeMap<String, u32> = BTreeMap::new();
        let mut budget_used = Budget::default();
        let mut first_admitted: Option<String> = None;

        for candidate in candidates {
            let is_first = first_admitted.is_none();
            match self.try_admit(
                &candidate,
                is_first,
                &owner_of_exclusive,
                &pool_used,
                &budget_used,
            ) {
                Ok(alloc) => {
                    // Commit the candidate's claims.
                    for ex in candidate.profile.exclusive.iter() {
                        if !self.ephemeral_dbs.contains(ex) {
                            owner_of_exclusive.insert(ex.clone(), candidate.id.clone());
                        }
                    }
                    for need in &candidate.profile.pooled {
                        *pool_used.entry(need.pool.clone()).or_insert(0) += need.count;
                    }
                    add_budget(&mut budget_used, &candidate.profile.fungible);
                    if first_admitted.is_none() {
                        first_admitted = Some(candidate.id.clone());
                    }
                    items.push(ScheduledItem {
                        id: candidate.id.clone(),
                        disposition: Disposition::Admitted,
                    });
                    allocations.push(alloc);
                }
                Err((behind_resource, reason)) => {
                    // Serialize behind the admitted owner of the conflicting
                    // resource when known, else behind the first admitted.
                    let behind = behind_resource
                        .and_then(|r| owner_of_exclusive.get(&r).cloned())
                        .or_else(|| first_admitted.clone())
                        .unwrap_or_else(|| candidate.id.clone());
                    items.push(ScheduledItem {
                        id: candidate.id.clone(),
                        disposition: Disposition::Serialized { behind, reason },
                    });
                }
            }
        }

        ConstraintPlan {
            admission: AdmissionPlan { items },
            allocations,
        }
    }

    /// Decide whether a candidate can be admitted given the running state, and if
    /// so produce its allocation. On refusal returns the conflicting resource (if
    /// any) and a human-readable reason.
    fn try_admit(
        &self,
        candidate: &Admission,
        is_first: bool,
        owner_of_exclusive: &BTreeMap<String, String>,
        pool_used: &BTreeMap<String, u32>,
        budget_used: &Budget,
    ) -> std::result::Result<Allocation, (Option<String>, String)> {
        if !is_first {
            // Hard-exclusive conflict (anything not an ephemeral-DB pool).
            for ex in &candidate.profile.exclusive {
                if !self.ephemeral_dbs.contains(ex) && owner_of_exclusive.contains_key(ex) {
                    return Err((
                        Some(ex.clone()),
                        format!("conflict on exclusive resource: {ex}"),
                    ));
                }
            }
            // Pool capacity.
            for need in &candidate.profile.pooled {
                let used = pool_used.get(&need.pool).copied().unwrap_or(0);
                let capacity = self.pools.get(&need.pool).map_or(0, |p| p.capacity);
                if used + need.count > capacity {
                    return Err((
                        None,
                        format!(
                            "pool '{}' exhausted (need {}, {} of {} used)",
                            need.pool, need.count, used, capacity
                        ),
                    ));
                }
            }
            // Host budget.
            if let Some(reason) =
                budget_overflow(budget_used, &candidate.profile.fungible, &self.host_budget)
            {
                return Err((None, reason));
            }
        }

        Ok(self.allocate(candidate, pool_used))
    }

    /// Build a candidate's concrete allocation (ports + ephemeral DBs + env).
    fn allocate(&self, candidate: &Admission, pool_used: &BTreeMap<String, u32>) -> Allocation {
        let mut ports: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        let mut local_used: BTreeMap<String, u32> = pool_used.clone();

        for need in &candidate.profile.pooled {
            let Some(pool) = self.pools.get(&need.pool) else {
                continue;
            };
            let used = local_used.entry(need.pool.clone()).or_insert(0);
            let mut assigned = Vec::new();
            for i in 0..need.count {
                let port = pool.base + *used + i;
                assigned.push(port);
                let key = format!("SPORK_{}_PORT_{}", env_key(&need.pool), i + 1);
                env.insert(key, port.to_string());
            }
            // A convenience binding for the conventional "ports" pool.
            if need.pool == "ports" {
                if let Some(first) = assigned.first() {
                    env.insert("PORT".into(), first.to_string());
                }
            }
            *used += need.count;
            ports.insert(need.pool.clone(), assigned);
        }

        let mut ephemeral_dbs: BTreeMap<String, String> = BTreeMap::new();
        for ex in &candidate.profile.exclusive {
            if self.ephemeral_dbs.contains(ex) {
                let db_name = format!("{}__{}", db_base(ex), db_suffix(&candidate.id));
                env.insert(format!("SPORK_DB_{}", env_key(ex)), db_name.clone());
                ephemeral_dbs.insert(ex.clone(), db_name);
            }
        }

        Allocation {
            id: candidate.id.clone(),
            ports,
            ephemeral_dbs,
            env,
        }
    }
}

impl Scheduler for ConstraintScheduler {
    fn admit(&self, candidates: Vec<Admission>) -> AdmissionPlan {
        self.plan(candidates).admission
    }
}

/// Add a candidate's budget into a running total (saturating).
fn add_budget(total: &mut Budget, add: &Budget) {
    total.cpu_milli = total.cpu_milli.saturating_add(add.cpu_milli);
    total.mem_mib = total.mem_mib.saturating_add(add.mem_mib);
    total.disk_mib = total.disk_mib.saturating_add(add.disk_mib);
}

/// Return a reason if adding `add` to `used` would exceed `cap`, else `None`.
fn budget_overflow(used: &Budget, add: &Budget, cap: &Budget) -> Option<String> {
    if used.cpu_milli.saturating_add(add.cpu_milli) > cap.cpu_milli {
        return Some(format!(
            "insufficient cpu (need {}m, {}m of {}m used)",
            add.cpu_milli, used.cpu_milli, cap.cpu_milli
        ));
    }
    if used.mem_mib.saturating_add(add.mem_mib) > cap.mem_mib {
        return Some(format!(
            "insufficient mem (need {}MiB, {}MiB of {}MiB used)",
            add.mem_mib, used.mem_mib, cap.mem_mib
        ));
    }
    if used.disk_mib.saturating_add(add.disk_mib) > cap.disk_mib {
        return Some(format!(
            "insufficient disk (need {}MiB, {}MiB of {}MiB used)",
            add.disk_mib, used.disk_mib, cap.disk_mib
        ));
    }
    None
}

/// Turn a resource/pool name into an uppercase env-key-safe token.
fn env_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The database base name for an ephemeral-DB exclusive (e.g.
/// `db:postgres-primary` -> `postgres_primary`).
fn db_base(exclusive: &str) -> String {
    let raw = exclusive.strip_prefix("db:").unwrap_or(exclusive);
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// A filesystem/identifier-safe suffix derived from a candidate id.
fn db_suffix(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{Budget, PooledNeed, ResourceProfile};

    fn cand(id: &str, profile: ResourceProfile) -> Admission {
        Admission::new(id, profile)
    }

    #[test]
    fn disjoint_exclusives_run_in_parallel() {
        let sched = ConstraintScheduler::new();
        let plan = sched.admit(vec![
            cand("a", ResourceProfile::new().with_exclusive("gpu:0")),
            cand("b", ResourceProfile::new().with_exclusive("gpu:1")),
        ]);
        // Both admitted — the whole point P7 adds over the serial scheduler.
        assert_eq!(plan.admitted(), vec!["a", "b"]);
        assert!(plan.serialized().is_empty());
    }

    #[test]
    fn shared_exclusive_serializes_with_a_precise_reason() {
        let sched = ConstraintScheduler::new();
        let p = ResourceProfile::new().with_exclusive("db:postgres-primary");
        let plan = sched.admit(vec![cand("a", p.clone()), cand("b", p)]);
        assert_eq!(plan.admitted(), vec!["a"]);
        let s = plan.serialized();
        assert_eq!(s.len(), 1);
        match &s[0].disposition {
            Disposition::Serialized { behind, reason } => {
                assert_eq!(behind, "a", "serialized behind the resource owner");
                assert!(reason.contains("db:postgres-primary"), "{reason}");
            }
            Disposition::Admitted => panic!("expected serialized"),
        }
    }

    #[test]
    fn empty_profiles_all_run_in_parallel() {
        let sched = ConstraintScheduler::new();
        let plan = sched.admit(vec![
            cand("a", ResourceProfile::new()),
            cand("b", ResourceProfile::new()),
            cand("c", ResourceProfile::new()),
        ]);
        assert_eq!(plan.admitted(), vec!["a", "b", "c"]);
    }

    #[test]
    fn ports_are_allocated_distinctly_and_rewritten_into_env() {
        let sched = ConstraintScheduler::new().with_pool("ports", PortPool::new(4000, 10));
        let need = ResourceProfile::new().with_pooled(PooledNeed::new("ports", 2));
        let plan = sched.plan(vec![cand("a", need.clone()), cand("b", need)]);
        assert_eq!(plan.admission.admitted(), vec!["a", "b"]);
        let a = &plan.allocations[0];
        let b = &plan.allocations[1];
        assert_eq!(a.ports["ports"], vec![4000, 4001]);
        assert_eq!(b.ports["ports"], vec![4002, 4003]);
        // No port collision and PORT env points at the first.
        assert_eq!(a.env["PORT"], "4000");
        assert_eq!(b.env["PORT"], "4002");
        assert_eq!(a.env["SPORK_PORTS_PORT_1"], "4000");
        assert_eq!(b.env["SPORK_PORTS_PORT_2"], "4003");
    }

    #[test]
    fn pool_exhaustion_serializes() {
        let sched = ConstraintScheduler::new().with_pool("ports", PortPool::new(4000, 3));
        let plan = sched.plan(vec![
            cand(
                "a",
                ResourceProfile::new().with_pooled(PooledNeed::new("ports", 2)),
            ),
            cand(
                "b",
                ResourceProfile::new().with_pooled(PooledNeed::new("ports", 2)),
            ),
        ]);
        assert_eq!(plan.admission.admitted(), vec!["a"]);
        match &plan.admission.serialized()[0].disposition {
            Disposition::Serialized { reason, .. } => {
                assert!(reason.contains("exhausted"), "{reason}")
            }
            Disposition::Admitted => panic!(),
        }
    }

    #[test]
    fn ephemeral_db_lets_both_siblings_run() {
        // Two siblings both "need the primary DB" — but it is registered as an
        // ephemeral-DB pool, so each gets its own database and they run parallel.
        let sched = ConstraintScheduler::new().with_ephemeral_db("db:postgres-primary");
        let p = ResourceProfile::new().with_exclusive("db:postgres-primary");
        let plan = sched.plan(vec![cand("branch-x", p.clone()), cand("branch-y", p)]);
        assert_eq!(plan.admission.admitted(), vec!["branch-x", "branch-y"]);
        let a = &plan.allocations[0];
        let b = &plan.allocations[1];
        let da = &a.ephemeral_dbs["db:postgres-primary"];
        let db = &b.ephemeral_dbs["db:postgres-primary"];
        assert_ne!(da, db, "each branch gets its own ephemeral DB");
        assert!(da.starts_with("postgres_primary__"));
        assert_eq!(a.env["SPORK_DB_DB_POSTGRES_PRIMARY"], *da);
    }

    #[test]
    fn budget_overflow_serializes_with_reason() {
        let sched = ConstraintScheduler::new().with_host_budget(Budget {
            cpu_milli: 3000,
            mem_mib: u64::MAX,
            disk_mib: u64::MAX,
        });
        let heavy = ResourceProfile::new().with_budget(Budget {
            cpu_milli: 2000,
            mem_mib: 0,
            disk_mib: 0,
        });
        let plan = sched.plan(vec![cand("a", heavy.clone()), cand("b", heavy)]);
        // a uses 2000, b would push to 4000 > 3000 -> serialized.
        assert_eq!(plan.admission.admitted(), vec!["a"]);
        match &plan.admission.serialized()[0].disposition {
            Disposition::Serialized { reason, .. } => assert!(reason.contains("cpu"), "{reason}"),
            Disposition::Admitted => panic!(),
        }
    }

    #[test]
    fn first_candidate_is_always_admitted() {
        // Even if it alone exceeds capacity, something must run (serial-parity
        // invariant); the executor surfaces a genuine over-allocation at run time.
        let sched = ConstraintScheduler::new().with_host_budget(Budget {
            cpu_milli: 100,
            mem_mib: 100,
            disk_mib: 100,
        });
        let huge = ResourceProfile::new().with_budget(Budget {
            cpu_milli: 9999,
            mem_mib: 9999,
            disk_mib: 9999,
        });
        let plan = sched.plan(vec![cand("only", huge)]);
        assert_eq!(plan.admission.admitted(), vec!["only"]);
    }

    #[test]
    fn empty_candidate_set_is_empty_plan() {
        let plan = ConstraintScheduler::new().plan(vec![]);
        assert!(plan.admission.items.is_empty());
        assert!(plan.allocations.is_empty());
    }
}
