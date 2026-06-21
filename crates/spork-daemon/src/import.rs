//! P7.5 MVP dispatch: `project.import` — capture the daemon's working tree into a
//! **root snapshot node** so a freshly opened project renders its code on the
//! canvas (DESIGN.md §10.1, §6.2, A.7 C-2).
//!
//! This is the MVP "unblocker". Every other wired capability (run-check, branch,
//! merge, restore, agent-run, history) operates on a *pre-existing* node, but
//! nothing minted the **first** node for a real repo — `open_project` rooted the
//! daemon and left an empty canvas, and the renderer cannot call
//! [`Command::NodeCreate`](spork_ipc::Command::NodeCreate) for a root because it
//! cannot mint a content hash. `project.import` closes that entirely behind the
//! frozen F3 seams: it reuses the daemon's own
//! [`capture_working_tree`](crate::Daemon::capture_working_tree) (feature.rs) and
//! the F2 graph `create_node` path, adds **no** event type, and carries the
//! versioned [`SnapshotPayload`] (origin = import|manual). Additive over the
//! frozen contracts (CLAUDE.md C2/C3).
//!
//! # The flow
//!
//! authorize (via capture) → content-address the working tree → build the
//! versioned snapshot payload (best-effort git metadata for an import) → create a
//! parentless snapshot-owning node → point the branch ref + `HEAD` at it →
//! publish `NODE_CREATED` + the ref events. An import on a branch that already has
//! a tip is refused (the renderer only imports an empty canvas; this guards an
//! accidental double-import — DESIGN.md A.7 C-2).

use spork_graph::RefKind;
use spork_ipc::{CommandResult, OpLogEvent};
use spork_nodes::{ImportSource, SnapshotOrigin, SnapshotPayload, SNAPSHOT_KIND};
use ulid::Ulid;

use crate::core::Daemon;
use crate::error::DaemonError;

/// The semver stamped on the imported root Snapshot node (matches
/// [`spork_nodes::type_version`]).
const SNAPSHOT_TYPE_VERSION: &str = "1.0.0";

impl Daemon {
    /// `project.import`: capture the working tree as a root snapshot node so the
    /// project renders on the canvas (DESIGN.md §10.1, §6.2).
    pub(crate) fn cmd_project_import(
        &self,
        branch_id: &str,
        origin: &str,
    ) -> Result<CommandResult, DaemonError> {
        let branch_id = if branch_id.is_empty() {
            "main"
        } else {
            branch_id
        };
        let origin = parse_origin(origin)?;

        // Idempotent on reopen: a branch that already has a tip was already
        // imported, and on reopen the graph is rebuilt from the durable log. The
        // renderer onboards by calling import every time it opens a project, so a
        // re-import must be a **no-op that returns the existing tip** — not an
        // error (which would leave the canvas stuck on the empty state) and not a
        // duplicate root. No new capture, no new node, no events.
        if let Some(existing) = self.branch_tip(branch_id)? {
            return Ok(self.record_mutation(serde_json::json!({
                "nodeId": existing.to_string(),
                "reopened": true,
            })));
        }

        // Capture the working tree into the CAS. This authorizes `snapshot.write`
        // and honors the ignore profile, so Spork's own `.spork/` state and the
        // dependency dirs never enter the store (DESIGN.md §10.5).
        let (snapshot_hash, _root_tree) = self.capture_working_tree()?;

        // Build the versioned snapshot payload (origin = import|manual). An import
        // records where the external state came from (DESIGN.md §7.1, A.7 C-2).
        let payload = self.build_snapshot_payload(origin);
        let payload_value = payload
            .to_value()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;

        // Create the parentless root node owning the captured snapshot.
        let version = semver::Version::parse(SNAPSHOT_TYPE_VERSION)
            .map_err(|e| DaemonError::Graph(format!("bad snapshot version: {e}")))?;
        let (node_id, schema_version) = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .create_node(
                    SNAPSHOT_KIND,
                    Some(&version),
                    Vec::new(), // a root node has no lineage parent
                    branch_id,
                    payload_value,
                    true,
                    Some(snapshot_hash),
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            (env.id, env.payload_schema_version)
        };

        // The resulting state flows out on the ordered rail, never on the return.
        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version,
        })?;

        // Point the branch ref + `HEAD` at the new root so it is a branch tip and
        // the canvas renders it as the current head (DESIGN.md §6.3).
        self.promote_ref(branch_id, node_id)?;
        self.set_head_ref(node_id)?;

        Ok(self.record_mutation(serde_json::json!({ "nodeId": node_id.to_string() })))
    }

    /// Point `HEAD` at `node_id`, creating the ref if it does not yet exist (a
    /// fresh import has no `HEAD`). The create-or-move existence check with the
    /// correct [`RefKind::Head`] — `move_ref` alone would no-op on a missing ref,
    /// so a brand-new graph needs the explicit create. `pub(crate)` so the P7.5
    /// edit loop (`crate::edit`) moves HEAD onto the new Edit node the same way.
    pub(crate) fn set_head_ref(&self, node_id: Ulid) -> Result<(), DaemonError> {
        let existed = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            let existed = core
                .graph
                .projection()
                .ref_target("HEAD")
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .is_some();
            if existed {
                core.graph
                    .move_ref("HEAD", node_id)
                    .map_err(|e| DaemonError::Graph(e.to_string()))?;
            } else {
                core.graph
                    .create_ref("HEAD", RefKind::Head, node_id)
                    .map_err(|e| DaemonError::Graph(e.to_string()))?;
            }
            existed
        };
        let ref_name = "HEAD".to_string();
        if existed {
            self.publish_event(|seq| OpLogEvent::RefMoved {
                seq,
                ref_name,
                to: node_id,
            })?;
        } else {
            self.publish_event(|seq| OpLogEvent::RefCreated { seq, ref_name })?;
        }
        Ok(())
    }

    /// The node a branch ref currently points at, or `None` if the ref is absent.
    fn branch_tip(&self, branch_id: &str) -> Result<Option<Ulid>, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        core.graph
            .projection()
            .ref_target(branch_id)
            .map_err(|e| DaemonError::Graph(e.to_string()))
    }

    /// Build the import/manual snapshot payload, enriching an import with
    /// best-effort git metadata when the working tree is a git repo (DESIGN.md
    /// §7.1 `importSource`, §10.4 non-invasive git read).
    fn build_snapshot_payload(&self, origin: SnapshotOrigin) -> SnapshotPayload {
        if origin != SnapshotOrigin::Import {
            return SnapshotPayload::manual();
        }
        let workdir = self.workdir();
        let uri = workdir.to_string_lossy().to_string();
        // A git repo contributes its HEAD ref; anything else imports as a
        // directory. The git read is non-invasive (DESIGN.md §10.4).
        let source = match self.import_git_state(&workdir) {
            Ok(ctx) => match ctx.head {
                Some(head) => ImportSource::new("git", uri).with_git_ref(head),
                None => ImportSource::new("directory", uri),
            },
            Err(_) => ImportSource::new("directory", uri),
        };
        SnapshotPayload::import(source)
    }
}

/// Parse the import origin token: empty/`"import"` → [`SnapshotOrigin::Import`],
/// `"manual"` → [`SnapshotOrigin::Manual`]. An unknown token is refused
/// (fail-closed), matching the daemon's other free-string parsers.
fn parse_origin(s: &str) -> Result<SnapshotOrigin, DaemonError> {
    match s {
        "" | "import" => Ok(SnapshotOrigin::Import),
        "manual" => Ok(SnapshotOrigin::Manual),
        other => Err(DaemonError::Graph(format!(
            "unknown import origin {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_origin_known_and_unknown() {
        assert_eq!(parse_origin("").unwrap(), SnapshotOrigin::Import);
        assert_eq!(parse_origin("import").unwrap(), SnapshotOrigin::Import);
        assert_eq!(parse_origin("manual").unwrap(), SnapshotOrigin::Manual);
        assert!(parse_origin("garbage").is_err());
    }
}
