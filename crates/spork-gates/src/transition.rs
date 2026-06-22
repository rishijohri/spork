//! The [`Transition`] a gate governs (DESIGN.md §8.3).

use serde::{Deserialize, Serialize};

/// The transition a [`GatePolicy`](crate::GatePolicy) governs.
///
/// The same declarative policy can guard different transitions; the transition
/// is recorded on the [`GateVerdict`](crate::GateVerdict) so an audit shows
/// exactly what was being gated (DESIGN.md §8.3). `#[non_exhaustive]` so a new
/// gated transition is an additive variant (CLAUDE.md C2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Transition {
    /// Merging a branch into another (the canonical post-merge gate — DESIGN.md
    /// A.4).
    Merge,
    /// Promoting a branch (making it the working line).
    PromoteBranch,
    /// Submitting a downstream/decision-tree task.
    SubmitDt,
    /// Creating a downstream/decision-tree task.
    CreateDt,
}

impl Transition {
    /// A short, stable label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Transition::Merge => "merge",
            Transition::PromoteBranch => "promote-branch",
            Transition::SubmitDt => "submit-dt",
            Transition::CreateDt => "create-dt",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_value(Transition::PromoteBranch).unwrap(),
            serde_json::json!("promote-branch")
        );
        assert_eq!(Transition::Merge.label(), "merge");
    }
}
