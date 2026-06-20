//! The auditable [`SelectionDecision`] trace.
//!
//! Developer trust in AI output is collapsing (only ~29% trust it), so context
//! assembly is **auditable**: every compile emits a `SelectionDecision` trace
//! explaining why each ancestor / layer was included, summarized, or dropped,
//! surfaced behind a single "expand context" affordance (DESIGN.md §13.6). This
//! is the defensible-debuggability story for when the context is wrong: the
//! trace and the assembled [`crate::CompiledContext`] always agree, layer for
//! layer.

use serde::{Deserialize, Serialize};

use crate::layer::ContextLayerKind;

/// The frozen schema version of the [`SelectionDecision`] shape (CLAUDE.md C5).
pub const SELECTION_DECISION_SCHEMA_VERSION: u16 = 1;

/// What the compiler decided to do with a candidate layer / ancestor.
///
/// The variants mirror the §13.6 audit vocabulary ("included, summarized, or
/// dropped") plus the §13.2 compaction case, so the trace explains every kind of
/// outcome a v1 compile can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// The candidate was included as-is.
    Included,
    /// The candidate was summarized before inclusion (an ancestor folded to a
    /// summary, or a handoff distillation).
    Summarized,
    /// The candidate was dropped (e.g. an
    /// [`AncestorStrategy::Drop`](crate::AncestorStrategy) policy, or a layer the
    /// source had no content for).
    Dropped,
    /// The candidate was compacted after the stable prefix to fit the budget
    /// without invalidating the warm cache (DESIGN.md §13.2).
    Compacted,
}

/// One entry in the compile's audit trace.
///
/// Each decision names the layer `kind`, the `source_ref` it concerns (the node
/// / memory id), the [`Disposition`], and a human-readable `reason` — exactly
/// what the "expand context" panel renders (DESIGN.md §13.6). `token_estimate`
/// records the size the decision accounted for, so the budget arithmetic is
/// itself auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionDecision {
    /// The schema version of this decision shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// The layer kind this decision concerns.
    pub kind: ContextLayerKind,
    /// The node / memory id the decision concerns.
    pub source_ref: String,
    /// What was done with the candidate.
    pub disposition: Disposition,
    /// A human-readable explanation, surfaced in the audit panel.
    pub reason: String,
    /// The token size this decision accounted for (0 for a drop).
    pub token_estimate: u64,
}

impl SelectionDecision {
    /// Construct a decision stamped with the current schema version.
    #[must_use]
    pub fn new(
        kind: ContextLayerKind,
        source_ref: impl Into<String>,
        disposition: Disposition,
        reason: impl Into<String>,
        token_estimate: u64,
    ) -> Self {
        SelectionDecision {
            schema_version: SELECTION_DECISION_SCHEMA_VERSION,
            kind,
            source_ref: source_ref.into(),
            disposition,
            reason: reason.into(),
            token_estimate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(SELECTION_DECISION_SCHEMA_VERSION, 1);
    }

    #[test]
    fn decision_round_trips_and_is_versioned() {
        let d = SelectionDecision::new(
            ContextLayerKind::Handoff,
            "node-7",
            Disposition::Summarized,
            "ancestor folded to handoff under handoff_only strategy",
            42,
        );
        assert_eq!(d.schema_version, SELECTION_DECISION_SCHEMA_VERSION);
        let v = serde_json::to_value(&d).unwrap();
        let back: SelectionDecision = serde_json::from_value(v).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn disposition_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(Disposition::Compacted).unwrap(),
            serde_json::json!("compacted")
        );
    }

    #[test]
    fn decision_canonicalizes_integers_only() {
        let d = SelectionDecision::new(
            ContextLayerKind::CurrentDiff,
            "node-1",
            Disposition::Included,
            "current diff",
            10,
        );
        assert!(spork_canon::canonicalize(&d).is_ok());
    }
}
