//! P7 historical checkout with fork-on-divergence (DESIGN.md §6.6).
//!
//! "Check out this node" materializes a node's exact state into the working tree
//! via the F4 dual-restore guard, then applies the **fork-on-divergence** policy
//! so a line is never silently overwritten: checking out a branch **tip**
//! continues that line (HEAD moves to it), while checking out a **non-tip** node
//! auto-forks a fresh branch at that node before moving HEAD (DESIGN.md §6.6). The
//! policy lives in the daemon — additive over the existing restore/branch-fork
//! seams, not a contract change (CLAUDE.md C2/C3).

use spork_broker::{Capability, RequestedScope};
use spork_graph::RefKind;
use spork_ipc::{CommandResult, OpLogEvent};
use ulid::Ulid;

use crate::core::{Daemon, WORKTREE_GLOB};
use crate::error::DaemonError;

impl Daemon {
    /// `node.checkout`: materialize a historical node and apply fork-on-divergence
    /// (DESIGN.md §6.6).
    ///
    /// A checkout of a non-tip node auto-forks a new branch at it (so subsequent
    /// work diverges onto its own line, never overwriting the original), then
    /// moves HEAD. A checkout of a tip simply moves HEAD. Emits
    /// `CHECKOUT_PERFORMED` (plus `BRANCH_FORKED`/`REF_CREATED` on a fork) and a
    /// HEAD `REF_MOVED`.
    pub(crate) fn cmd_node_checkout(&self, node_id: Ulid) -> Result<CommandResult, DaemonError> {
        // Checkout materializes the snapshot into the working dir — snapshot.write.
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        // §6.6 tip-ness: a node is a tip iff no other node continues it (has it as
        // a parent). A non-tip checkout must fork so the line is not overwritten.
        let is_tip = self.node_is_tip(node_id)?;

        // Materialize the node's exact code (+ bound conversation) via the atomic
        // dual-restore guard — fail-closed, forward history preserved.
        let outcome = self.guarded_restore(node_id)?;
        debug_assert_eq!(outcome.node_id, node_id);

        // Fork-on-divergence: a non-tip checkout lands future work on a fresh
        // branch at this node rather than overwriting the existing line.
        let forked_ref = if is_tip {
            None
        } else {
            let name = format!("checkout-{}", checkout_suffix(node_id));
            let ref_id = self.guarded_branch_fork(node_id, &name)?;
            let ref_name = ref_id.as_str().to_string();
            let forked = ref_name.clone();
            self.publish_event(|seq| OpLogEvent::BranchForked {
                seq,
                ref_name: forked,
            })?;
            let created = ref_name.clone();
            self.publish_event(|seq| OpLogEvent::RefCreated {
                seq,
                ref_name: created,
            })?;
            Some(ref_name)
        };

        // Move HEAD to the checked-out node.
        self.move_head_to(node_id)?;
        self.publish_event(|seq| OpLogEvent::CheckoutPerformed { seq, node_id })?;

        Ok(self.record_mutation(serde_json::json!({
            "nodeId": node_id.to_string(),
            "isTip": is_tip,
            "forkedRef": forked_ref,
        })))
    }

    /// Whether `node_id` is a branch tip — no **lineage** (code-changing) work
    /// continues it (DESIGN.md §6.6). Observing/context children (a sanity result,
    /// a gate verdict, an agent-context node) are attachments, not continuations,
    /// so they do not make a node a non-tip — only a [`Family::Mutating`] child
    /// does. A non-existent node is an error. `pub(crate)` so the P7.5 edit loop
    /// (`crate::edit`) applies the same §6.6 fork-on-divergence decision.
    pub(crate) fn node_is_tip(&self, node_id: Ulid) -> Result<bool, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let state = core
            .graph
            .projection()
            .state()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        if !state.nodes.contains_key(&node_id.to_string()) {
            return Err(DaemonError::NotFound(format!("node {node_id}")));
        }
        let has_lineage_child = state.nodes.values().any(|env| {
            env.parent_ids.contains(&node_id) && env.family == spork_graph::Family::Mutating
        });
        Ok(!has_lineage_child)
    }

    /// Move HEAD to a node, creating the HEAD ref if it does not yet exist.
    fn move_head_to(&self, node_id: Ulid) -> Result<(), DaemonError> {
        {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .move_ref("HEAD", node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))
                .or_else(|_| {
                    core.graph
                        .create_ref("HEAD", RefKind::Head, node_id)
                        .map_err(|e| DaemonError::Graph(e.to_string()))
                })?;
        }
        self.publish_event(|seq| OpLogEvent::RefMoved {
            seq,
            ref_name: "HEAD".to_string(),
            to: node_id,
        })?;
        Ok(())
    }
}

/// A short, stable suffix derived from a node id for the auto-forked branch name.
fn checkout_suffix(node_id: Ulid) -> String {
    let s = node_id.to_string();
    // The last 8 chars of the ULID are stable and collision-resistant enough for a
    // human-facing branch label; the full id still uniquely keys the node.
    s.chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_suffix_is_eight_chars() {
        let id = Ulid::new();
        let suffix = checkout_suffix(id);
        assert_eq!(suffix.len(), 8);
        assert!(id.to_string().ends_with(&suffix));
    }
}
