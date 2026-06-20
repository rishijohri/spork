//! The admission seam: [`Scheduler`], [`Admission`], [`AdmissionPlan`], and the
//! v1 [`SerialScheduler`].
//!
//! Parallel branches are NOT free — worktrees share databases, the Docker
//! daemon, and fixed ports, so admitting siblings blindly causes races and
//! flaky results (DESIGN.md §5.3, §11.2). `Scheduler.admit(candidates)` is
//! therefore a *constraint-solving admission decision*: siblings whose resource
//! sets are disjoint run in parallel; otherwise they are serialized and the DAG
//! UI surfaces a **queued-with-reason** state ("conflict on
//! `db:postgres-primary`") so the wait is honest, not mysterious. **Unknown
//! stateful services default to exclusive** — the conservative choice that
//! prevents silent cross-branch contamination (DESIGN.md §11.2).
//!
//! Per the foundation discipline (CLAUDE.md C3) F4 ships exactly one
//! implementation, [`SerialScheduler`]: it **admits at most one** candidate and
//! serializes the rest *with a recorded reason*. This honors the product's
//! explicit hedge that parallel testing is not always possible by making the
//! constraint a first-class scheduling decision rather than a silent source of
//! flakiness. The full constraint solver (true disjoint-set parallelism, pooled
//! port allocation, fungible budgeting) is P7, added behind this same trait.
//!
//! Design references: DESIGN.md §5.3 (constraint-based scheduler), §11.2
//! (resource-aware scheduling: parallel vs serialized; unknown stateful services
//! default to exclusive).

use serde::{Deserialize, Serialize};

use crate::resource::ResourceProfile;

/// A candidate offered to the scheduler for admission.
///
/// Pairs a stable identifier (typically a node id, kept as a string here so the
/// seam does not depend on the graph crate) with the
/// [`ResourceProfile`](crate::ResourceProfile) the candidate declares. The order
/// candidates are presented in is significant for the v1 serial scheduler: it is
/// the priority order in which the queue is built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admission {
    /// A stable identifier for the candidate (e.g. a node id).
    pub id: String,
    /// The resources this candidate declares it needs.
    pub profile: ResourceProfile,
}

impl Admission {
    /// Construct a candidate from an id and its resource profile.
    #[must_use]
    pub fn new(id: impl Into<String>, profile: ResourceProfile) -> Self {
        Admission {
            id: id.into(),
            profile,
        }
    }
}

/// The disposition of one candidate in an [`AdmissionPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// The candidate is admitted to run now.
    Admitted,
    /// The candidate is serialized (queued) behind another, with a
    /// human-readable reason for the honest queued-with-reason UI (DESIGN.md
    /// §11.2).
    Serialized {
        /// The id of the candidate this one waits behind.
        behind: String,
        /// Why it was serialized (e.g. a resource-conflict description, or the
        /// serial-policy default).
        reason: String,
    },
}

impl Disposition {
    /// Whether this disposition admitted the candidate to run now.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        matches!(self, Disposition::Admitted)
    }
}

/// One scheduled candidate: its id and the disposition the scheduler assigned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledItem {
    /// The candidate's id.
    pub id: String,
    /// What the scheduler decided for it.
    pub disposition: Disposition,
}

/// The result of [`Scheduler::admit`]: every candidate with its disposition.
///
/// Items appear in the order they were presented. Exactly the admitted items may
/// run now; the serialized items carry a reason explaining the wait so the DAG UI
/// can render an honest queued-with-reason state (DESIGN.md §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionPlan {
    /// Every candidate, in presentation order, with its disposition.
    pub items: Vec<ScheduledItem>,
}

impl AdmissionPlan {
    /// The ids admitted to run now, in order.
    #[must_use]
    pub fn admitted(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter(|i| i.disposition.is_admitted())
            .map(|i| i.id.as_str())
            .collect()
    }

    /// The serialized (queued) items, in order.
    #[must_use]
    pub fn serialized(&self) -> Vec<&ScheduledItem> {
        self.items
            .iter()
            .filter(|i| !i.disposition.is_admitted())
            .collect()
    }
}

/// The admission seam (DESIGN.md §5.3, §11.2).
///
/// `admit` takes the candidate siblings competing for resources and returns a
/// plan saying which run now and which are serialized (with a reason). F4 ships
/// exactly one implementation ([`SerialScheduler`]); the full constraint solver
/// is a new implementation of this trait in P7 (CLAUDE.md C3).
pub trait Scheduler {
    /// Decide which candidates run now and which are serialized.
    fn admit(&self, candidates: Vec<Admission>) -> AdmissionPlan;
}

/// The v1 serial scheduler: admit at most one, serialize the rest with a reason
/// (DESIGN.md §11.2).
///
/// This is the deliberately-conservative F4 implementation: it admits the first
/// candidate and serializes every other one behind it. The reason it records
/// distinguishes a *resource conflict* with the admitted candidate (a shared
/// exclusive singleton, named precisely) from the *serial-policy default* that
/// applies to everything else — because F4 runs one workspace at a time, and
/// because unknown stateful services default to exclusive (DESIGN.md §11.2), the
/// safe answer is to serialize. Parallelism over disjoint resource sets is P7.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerialScheduler;

impl SerialScheduler {
    /// Construct a serial scheduler.
    #[must_use]
    pub fn new() -> Self {
        SerialScheduler
    }
}

impl Scheduler for SerialScheduler {
    fn admit(&self, candidates: Vec<Admission>) -> AdmissionPlan {
        let mut items = Vec::with_capacity(candidates.len());
        let mut admitted: Option<Admission> = None;

        for candidate in candidates {
            match &admitted {
                None => {
                    // The first candidate is admitted to run now.
                    items.push(ScheduledItem {
                        id: candidate.id.clone(),
                        disposition: Disposition::Admitted,
                    });
                    admitted = Some(candidate);
                }
                Some(running) => {
                    // Everything else is serialized. Prefer a precise
                    // resource-conflict reason when one exists; otherwise fall
                    // back to the serial-policy reason (F4 runs one at a time;
                    // unknown stateful services default to exclusive).
                    let overlap = running.profile.exclusive_overlap(&candidate.profile);
                    let reason = if overlap.is_empty() {
                        "serialized by serial scheduler (F4 admits one workspace at a time; \
                         unknown stateful services default to exclusive — the full \
                         constraint solver lands in P7)"
                            .to_string()
                    } else {
                        format!("conflict on exclusive resource(s): {}", overlap.join(", "))
                    };
                    items.push(ScheduledItem {
                        id: candidate.id.clone(),
                        disposition: Disposition::Serialized {
                            behind: running.id.clone(),
                            reason,
                        },
                    });
                }
            }
        }

        AdmissionPlan { items }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceProfile;

    fn cand(id: &str, profile: ResourceProfile) -> Admission {
        Admission::new(id, profile)
    }

    #[test]
    fn empty_candidate_set_yields_empty_plan() {
        let plan = SerialScheduler::new().admit(vec![]);
        assert!(plan.items.is_empty());
        assert!(plan.admitted().is_empty());
    }

    #[test]
    fn single_candidate_is_admitted() {
        let plan = SerialScheduler::new().admit(vec![cand("a", ResourceProfile::new())]);
        assert_eq!(plan.admitted(), vec!["a"]);
        assert!(plan.serialized().is_empty());
    }

    #[test]
    fn serial_scheduler_admits_at_most_one_and_serializes_the_rest() {
        // Even three resource-disjoint candidates are serialized in F4 — the
        // serial scheduler parallelizes nothing (one at a time).
        let plan = SerialScheduler::new().admit(vec![
            cand("a", ResourceProfile::new()),
            cand("b", ResourceProfile::new()),
            cand("c", ResourceProfile::new()),
        ]);
        assert_eq!(plan.admitted(), vec!["a"], "exactly one admitted");
        let serialized = plan.serialized();
        assert_eq!(serialized.len(), 2);
        for item in serialized {
            match &item.disposition {
                Disposition::Serialized { behind, reason } => {
                    assert_eq!(behind, "a");
                    assert!(!reason.is_empty(), "every serialized item carries a reason");
                }
                Disposition::Admitted => panic!("expected serialized"),
            }
        }
    }

    #[test]
    fn conflicting_candidates_serialize_with_a_precise_resource_reason() {
        let conflicting = ResourceProfile::new().with_exclusive("db:postgres-primary");
        let plan = SerialScheduler::new()
            .admit(vec![cand("a", conflicting.clone()), cand("b", conflicting)]);
        assert_eq!(plan.admitted(), vec!["a"]);
        let serialized = plan.serialized();
        assert_eq!(serialized.len(), 1);
        match &serialized[0].disposition {
            Disposition::Serialized { behind, reason } => {
                assert_eq!(behind, "a");
                assert!(
                    reason.contains("db:postgres-primary"),
                    "reason names the conflicting resource: {reason}"
                );
            }
            Disposition::Admitted => panic!("expected serialized"),
        }
    }

    #[test]
    fn disjoint_candidates_still_serialize_in_f4_with_the_policy_reason() {
        let plan = SerialScheduler::new().admit(vec![
            cand("a", ResourceProfile::new().with_exclusive("gpu:0")),
            cand("b", ResourceProfile::new().with_exclusive("gpu:1")),
        ]);
        let serialized = plan.serialized();
        assert_eq!(serialized.len(), 1);
        match &serialized[0].disposition {
            Disposition::Serialized { reason, .. } => {
                assert!(
                    reason.contains("serial scheduler"),
                    "disjoint candidates serialize via the serial-policy reason: {reason}"
                );
            }
            Disposition::Admitted => panic!("expected serialized"),
        }
    }

    #[test]
    fn plan_round_trips_through_serde() {
        let plan = SerialScheduler::new().admit(vec![
            cand("a", ResourceProfile::new()),
            cand("b", ResourceProfile::new().with_exclusive("x")),
        ]);
        let json = serde_json::to_string(&plan).unwrap();
        let back: AdmissionPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }
}
