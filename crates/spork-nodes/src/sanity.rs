//! The **Sanity / Pattern-Check** built-in node type (DESIGN.md §7.1, §8.1,
//! §8.2).
//!
//! A Sanity node is the deterministic *observing* kind that **auto-runs after
//! every agentic edit** (DESIGN.md §8.2, `onEditNodeCommitted`): it is
//! change-scoped (only the edit's changed paths are scanned), debounced, and
//! hermetic (no network, read-only base, pinned rules), and it emits
//! [`Violation`](spork_runner::Violation)s on the one
//! [`ResultEnvelope`](spork_runner::ResultEnvelope) without mutating the parent.
//!
//! # Reuse, don't re-implement
//!
//! The actual scan logic is the F4 [`SanityRunner`](spork_runner::SanityRunner) —
//! the single deterministic lint/pattern runner already shipped behind the one
//! Runner SPI (DESIGN.md §8.1). This crate does **not** re-implement it; it
//! *reuses* it ([`runner`]) and adds only the P5 surface: the node
//! [`descriptor`] (so a Sanity node registers through the same public registry as
//! every other built-in) and the [`SanityPayload`] that records the auto-run
//! flag and the target node. This is the dogfooding rule — built-ins and the
//! frozen F4 runner share one path (DESIGN.md §7.1, §9).
//!
//! # Auto-run / cache-hit on unchanged subtrees
//!
//! Because the scan is deterministic and runs through the F4 input-digest cache,
//! an identical `(spec, input_tree, runner_version, change_scope)` re-run is a
//! cache hit and is not re-executed (DESIGN.md §8.1, §9.2). The change scope
//! lives in the [`SandboxContext`](spork_runner::SandboxContext) the daemon
//! builds from the edit's `files_changed`, so an edit that touched a subtree
//! shared by a sibling branch cache-hits on the unchanged part.
//!
//! Design references: DESIGN.md §7.1 (taxonomy), §8.1 (the SanityRunner behind
//! the one SPI; violations), §8.2 (auto-run, change-scoped, hermetic), §9.2
//! (deterministic ⇒ cacheable).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_graph::EdgeType;
use spork_registry::{Family, NodeTypeDescriptor, StalenessRule};
use spork_runner::{SanityRunner, SANITY_KIND};

use crate::descriptor::type_version;

/// The check kind the sanity node uses — the *same* kind the F4
/// [`SanityRunner`](spork_runner::SanityRunner) handles, so the built-in node and
/// the frozen runner share one path (DESIGN.md §8.1).
pub const SANITY_NODE_KIND: &str = SANITY_KIND;

/// The schema version stamped on a freshly built [`SanityPayload`].
pub const SANITY_PAYLOAD_VERSION: u16 = 1;

/// Build the deterministic sanity [`Runner`](spork_runner::Runner) — the reused
/// F4 [`SanityRunner`](spork_runner::SanityRunner).
///
/// P5 does not ship a second sanity runner; it reuses the one frozen in F4
/// (DESIGN.md §8.1), so the auto-run check, the on-demand check, and any future
/// custom sanity caller all share one deterministic, hermetic implementation.
#[must_use]
pub fn runner() -> SanityRunner {
    SanityRunner::new()
}

/// The schema-versioned payload of a Sanity node (DESIGN.md §7.1).
///
/// `target_node_id` is the node whose snapshot the check observes; `autorun`
/// records whether this check was scheduled by the Edit auto-run hook (DESIGN.md
/// §8.2); `config` is the deterministic rule set the
/// [`SanityRunner`](spork_runner::SanityRunner) interprets (forbidden patterns,
/// max line length, include/exclude globs). The per-run violations live on the
/// append-only [`ResultEnvelope`](spork_runner::ResultEnvelope), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanityPayload {
    /// The schema version of this payload ([`SANITY_PAYLOAD_VERSION`]).
    pub schema_version: u16,
    /// The node this sanity check observes (its snapshot is never mutated).
    pub target_node_id: String,
    /// Whether this check was auto-scheduled by the Edit auto-run hook (DESIGN.md
    /// §8.2) as opposed to run on demand.
    pub autorun: bool,
    /// The deterministic rule set the
    /// [`SanityRunner`](spork_runner::SanityRunner) interprets.
    pub config: Value,
}

impl SanityPayload {
    /// Construct an auto-run sanity payload targeting `target_node_id` with the
    /// given rule-set `config`.
    ///
    /// The auto-run flag defaults to `true` because the headline use is the
    /// `onEditNodeCommitted` hook (DESIGN.md §8.2); flip it with
    /// [`on_demand`](SanityPayload::on_demand) for a manually triggered check.
    #[must_use]
    pub fn new(target_node_id: impl Into<String>, config: Value) -> Self {
        SanityPayload {
            schema_version: SANITY_PAYLOAD_VERSION,
            target_node_id: target_node_id.into(),
            autorun: true,
            config,
        }
    }

    /// Mark this check as on-demand (not auto-scheduled) (builder style).
    #[must_use]
    pub fn on_demand(mut self) -> Self {
        self.autorun = false;
        self
    }

    /// Render this payload to the JSON `payload` value the daemon stores.
    ///
    /// # Errors
    /// [`NodesError::Serialize`](crate::NodesError::Serialize) on an encoding
    /// failure.
    pub fn to_value(&self) -> crate::Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// Build the Sanity/Pattern-Check [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Observing`] type owning no snapshot, originating a
/// [`EdgeType::Checks`] edge to the Edit it observes (DESIGN.md §6.3). It is
/// hermetic by contract (no network, read-only base — DESIGN.md §8.2), so it
/// requires only `snapshot.read`, not `process.spawn`: the scan is in-process
/// over the materialized tree.
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: SANITY_NODE_KIND.to_string(),
        type_version: type_version(),
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": { "type": "integer", "minimum": 1 },
                "target_node_id": { "type": "string" },
                "autorun": { "type": "boolean" },
                "config": { "type": "object" }
            },
            "required": ["schema_version", "target_node_id", "autorun", "config"]
        }),
        result_schema: Some(json!({
            "type": "object",
            "description": "ResultEnvelope with lint/pattern violations (DESIGN §8.1)"
        })),
        allowed_edges: vec![EdgeType::Checks],
        ports: vec![],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.read".to_string()],
        ui_contributions: json!({ "color": "#f59e0b", "icon": "shield-check", "displayName": "Sanity" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_registry::NodeTypeRegistry;
    use spork_runner::Runner;

    #[test]
    fn descriptor_is_observing_and_hermetic() {
        let d = descriptor();
        assert_eq!(d.id, SANITY_NODE_KIND);
        assert_eq!(d.family, Family::Observing);
        assert!(!d.owns_snapshot);
        // Hermetic: no process.spawn capability is required (DESIGN §8.2).
        assert!(!d.capabilities_required.iter().any(|c| c == "process.spawn"));
        assert!(d.capabilities_required.iter().any(|c| c == "snapshot.read"));
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn runner_is_the_reused_f4_sanity_runner() {
        // The built-in reuses the frozen F4 SanityRunner (DESIGN §8.1), so it
        // describes itself with the F4 sanity identity, not a P5-bespoke one.
        let caps = runner().describe();
        assert!(caps.handles(SANITY_KIND));
        assert!(caps.hermetic);
        assert_eq!(caps.name, "sanity");
    }

    #[test]
    fn payload_defaults_to_autorun_and_round_trips() {
        let p = SanityPayload::new("01H...", json!({ "forbid": ["FIXME"] }));
        assert_eq!(p.schema_version, SANITY_PAYLOAD_VERSION);
        assert!(p.autorun, "the headline use is the auto-run hook");
        let v = p.to_value().unwrap();
        let back: SanityPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn on_demand_clears_the_autorun_flag() {
        let p = SanityPayload::new("01H...", json!({})).on_demand();
        assert!(!p.autorun);
    }

    #[test]
    fn sanity_node_kind_is_the_runner_kind() {
        // The node and the runner agree on the kind, so dispatch routes to the
        // reused runner with no special-casing.
        assert_eq!(SANITY_NODE_KIND, runner().describe().kinds[0]);
    }
}
