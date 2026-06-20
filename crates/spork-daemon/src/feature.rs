//! The daemon's non-IPC-command capabilities: drift capture, git coexistence,
//! the credential vault, and the event/ephemeral subscriptions.
//!
//! Not every F3 capability is an IPC [`Command`](spork_ipc::Command). The frozen
//! command enum is the *renderer-driven* surface; the daemon additionally owns
//! out-of-band machinery the headless client drives directly to exercise the full
//! DoD:
//!
//! - **Drift capture** ([`Daemon::capture_drift`]) — the daemon fuses change
//!   sources, attributes each change (A.3 precedence), secret-scans at capture
//!   (so a key never enters the CAS), and records an `origin = auto_drift`
//!   snapshot node, emitting its `NODE_CREATED`/`EDGE_ADDED` on the ordered rail
//!   like any other node (DESIGN.md §10.1, §10.2, §15.5, A.3).
//! - **Git coexistence** ([`Daemon::import_git_state`], [`Daemon::export_to_git`])
//!   — non-invasive read of `.git` state, and projection of a node tree into a
//!   new git branch without ever mutating the user's HEAD/index/working tree
//!   (DESIGN.md §10.4).
//! - **Credential vault** ([`Daemon::vault_put`], [`Daemon::vault_get`],
//!   [`Daemon::vault_delete`]) — secrets resolved only inside the daemon, gated
//!   on the `secrets.get` capability, never hashed into the CAS (DESIGN.md
//!   §15.4). The renderer holds zero secrets and only ever sees a [`VaultRef`].
//! - **Subscriptions** ([`Daemon::subscribe_events`], [`Daemon::subscribe_node`])
//!   — the ordered durable rail and the node-keyed ephemeral side-channels.
//!
//! Design references: DESIGN.md §10.1, §10.2, §10.4, §14.4, §15.4, A.3.

use std::path::Path;

use crossbeam_channel::Receiver;
use spork_broker::{Capability, RequestedScope};
use spork_drift::{AttributionRecord, BufferBridge, ChangeSource, DriftCaptureReport, TurnContext};
use spork_git::{import_git_state as git_import, GitContext};
use spork_ipc::{EphemeralChannel, EphemeralFrame, OpLogEvent};
use spork_vault::{CredentialVault, Secret, VaultRef};
use ulid::Ulid;

use crate::core::{Daemon, WORKTREE_GLOB};
use crate::error::DaemonError;

/// Re-export the transport receiver type so a headless client can name a
/// subscription's return type without depending on `crossbeam-channel` directly.
pub use crossbeam_channel::Receiver as EventReceiver;

impl Daemon {
    /// Subscribe to the ordered durable [`OpLogEvent`] stream.
    ///
    /// Every mutation's resulting state arrives here, in `seq` order with no
    /// gaps — the single reconciliation path the renderer reduces over
    /// (DESIGN.md §5.5, §14.4). Subscribe *before* issuing mutations to observe
    /// their events. This is the frozen subscription half of the view-model
    /// boundary (alongside [`Daemon::graph_view`](crate::Daemon::graph_view)).
    #[must_use]
    pub fn subscribe_events(&self) -> Receiver<OpLogEvent> {
        self.events.subscribe()
    }

    /// Subscribe to a node's ephemeral side-channel (chat tokens / run stdout).
    ///
    /// Ephemeral frames are off the ordered stream and keyed by node id, so a
    /// flood here can never stall ordered op-log delivery (DESIGN.md §5.5,
    /// §14.4). The transport (`spork-stream`) enforces that non-interference.
    #[must_use]
    pub fn subscribe_node(&self, node_id: Ulid) -> Receiver<EphemeralFrame> {
        self.ephemeral.subscribe(node_id)
    }

    /// Publish one ephemeral frame (a chat token or a line of run stdout) for a
    /// node onto its side-channel.
    ///
    /// This is the producer side of the ephemeral channel — the daemon (or a
    /// runner it supervises) calls it as tokens/stdout arrive. It never touches
    /// the ordered rail.
    pub fn publish_ephemeral(
        &self,
        node_id: Ulid,
        channel: EphemeralChannel,
        data: impl Into<String>,
    ) {
        self.ephemeral
            .publish(EphemeralFrame::new(node_id, channel, data));
    }

    /// Fuse change sources and attribute every change under `ctx`, then capture
    /// the working tree as an `origin = auto_drift` snapshot node — excluding any
    /// secret-bearing file so it never enters the CAS (DESIGN.md §10.2, §15.5,
    /// A.3).
    ///
    /// The flow is authorize → fuse+attribute → secret-scan+store+node →
    /// publish events:
    /// 1. Clear `snapshot.write` (capture writes a snapshot binding).
    /// 2. `fuse_and_attribute` drains the sources and applies the A.3 precedence
    ///    (interceptor outranks watcher; rescan only adds).
    /// 3. `capture` stages only non-secret files, stores them, and creates the
    ///    drift node through the graph service.
    /// 4. The new node's `NODE_CREATED` (+ `EDGE_ADDED` per parent) are published
    ///    onto the ordered rail, exactly like a `node.create`.
    ///
    /// Returns the drift report (node id, snapshot/tree hashes, attribution
    /// records, and the excluded-secret path list).
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `snapshot.write` is denied.
    /// - [`DaemonError::Drift`] on a capture/store/graph failure.
    pub fn capture_drift(
        &self,
        sources: &mut [&mut dyn ChangeSource],
        ctx: &TurnContext,
        parents: Vec<Ulid>,
        branch_id: &str,
        buffers: Option<&BufferBridge>,
    ) -> Result<DriftCaptureReport, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        let report = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            // Split the borrow: `fuse_and_attribute` and `capture` both live on
            // `drift`, but `capture` also needs `&store` and `&mut graph`.
            let attribution: Vec<AttributionRecord> = core.drift.fuse_and_attribute(sources, ctx);
            let DaemonCoreParts {
                drift,
                store,
                graph,
            } = core.drift_capture_parts();
            drift
                .capture(
                    store,
                    graph,
                    parents.clone(),
                    branch_id,
                    attribution,
                    buffers,
                )
                .map_err(|e| DaemonError::Drift(e.to_string()))?
        };

        // Surface the drift node on the ordered rail like any other node.
        let node_id = report.node_id;
        let env = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .get_node(node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
        };
        let schema_version = env.map_or(1, |e| e.payload_schema_version);
        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version,
        })?;
        for parent in &parents {
            let parent = *parent;
            self.publish_event(|seq| OpLogEvent::EdgeAdded {
                seq,
                from: parent,
                to: node_id,
                edge: spork_graph::EdgeType::ParentChild,
            })?;
        }

        Ok(report)
    }

    /// Capture the current working tree into the CAS and return the
    /// `(snapshot_hash, root_tree_hash)` for a node that will own it.
    ///
    /// This is the daemon's own snapshot-capture path (the trusted-core analogue
    /// of the edit-interceptor write): it walks the working directory honoring the
    /// daemon's ignore profile — the same exclusion machinery that defines
    /// snapshot identity (DESIGN.md §10.5) — stores every blob/tree, and binds a
    /// [`spork_cas::Snapshot`] over the root tree. The returned `snapshot_hash`
    /// is what a [`Command::NodeCreate`](spork_ipc::Command::NodeCreate) passes as
    /// its `snapshot_hash`, so the resulting node is restorable/diffable and
    /// exportable to git.
    ///
    /// Gated on `snapshot.write` (capture writes content to the store).
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `snapshot.write` is denied.
    /// - [`DaemonError::Cas`] on a walk/store failure.
    pub fn capture_working_tree(
        &self,
    ) -> Result<(spork_hash::Hash, spork_hash::Hash), DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let ignore_profile_hash = core.ignore_profile.hash();
        let (snapshot_hash, root_tree, _stats) = core
            .store
            .capture_snapshot(&core.workdir, &core.matcher, ignore_profile_hash, None)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok((snapshot_hash, root_tree))
    }

    /// Store conversation bytes in the CAS and return the content hash that a
    /// node's payload binds as its `conversationRef` (DESIGN.md §10.3).
    ///
    /// The canonical transcript *schema* is an F4 concern; here the daemon freezes
    /// only the ref slot — the bound conversation is an opaque content hash the
    /// restore guard verifies is present before it materializes code, so a restore
    /// is atomic across code and conversation (DESIGN.md §6.4, §10.3, §11.4).
    ///
    /// Gated on `snapshot.write`.
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `snapshot.write` is denied.
    /// - [`DaemonError::Cas`] on a store failure.
    pub fn put_conversation(&self, bytes: &[u8]) -> Result<spork_hash::Hash, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let (hash, _stats) = core
            .store
            .put_blob_bytes(bytes)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok(hash)
    }

    /// Import the user's git state non-invasively: HEAD/branch/dirty as metadata,
    /// with `.git`/index/working-tree untouched (DESIGN.md §10.4).
    ///
    /// # Errors
    /// Returns [`DaemonError::Git`] if `repo_path` is not a repository or a git
    /// read fails.
    pub fn import_git_state(&self, repo_path: impl AsRef<Path>) -> Result<GitContext, DaemonError> {
        git_import(repo_path).map_err(|e| DaemonError::Git(e.to_string()))
    }

    /// Export a node's snapshot tree into a new git branch, leaving the user's
    /// HEAD, index, and working tree byte-unchanged (DESIGN.md §10.4).
    ///
    /// Resolves the node's snapshot root tree, then writes the equivalent git
    /// blobs/trees and a parentless commit on a new `refs/heads/<branch_name>`.
    /// Returns the new commit's SHA-1.
    ///
    /// # Errors
    /// - [`DaemonError::NotFound`] if the node does not exist or owns no snapshot.
    /// - [`DaemonError::Git`] on a git write failure (including the branch
    ///   already existing — a non-force create).
    pub fn export_to_git(
        &self,
        repo_path: impl AsRef<Path>,
        node_id: Ulid,
        branch_name: &str,
    ) -> Result<String, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let env = core
            .graph
            .get_node(node_id)
            .map_err(|e| DaemonError::Graph(e.to_string()))?
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
        let snapshot_hash = env
            .snapshot_hash
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id} owns no snapshot")))?;
        let snapshot = core
            .store
            .read_snapshot(&snapshot_hash)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        spork_git::export_to_git(repo_path, &core.store, snapshot.root_tree, branch_name)
            .map_err(|e| DaemonError::Git(e.to_string()))
    }

    /// Store a secret in the vault, returning the opaque [`VaultRef`] that is the
    /// only thing ever persisted/referenced (DESIGN.md §15.4).
    ///
    /// Gated on `secrets.get` — storing a secret is a credential operation the
    /// broker must authorize. The [`Secret`] is zeroized on drop and cannot be
    /// serialized into the CAS by construction.
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `secrets.get` is denied.
    /// - [`DaemonError::Vault`] on a vault write failure.
    pub fn vault_put(&self, name: &str, secret: Secret) -> Result<VaultRef, DaemonError> {
        self.authorize(Capability::SecretsGet, &RequestedScope::none())?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        core.vault
            .put(name, secret)
            .map_err(|e| DaemonError::Vault(e.to_string()))
    }

    /// Resolve a [`VaultRef`] back to its [`Secret`], inside the daemon only.
    ///
    /// Gated on `secrets.get`. The returned [`Secret`] never leaves the daemon's
    /// trust boundary in serialized form (it does not implement `Serialize`).
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `secrets.get` is denied.
    /// - [`DaemonError::Vault`] if the ref is unknown or a read fails.
    pub fn vault_get(&self, vault_ref: &VaultRef) -> Result<Secret, DaemonError> {
        self.authorize(Capability::SecretsGet, &RequestedScope::none())?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        core.vault
            .get(vault_ref)
            .map_err(|e| DaemonError::Vault(e.to_string()))
    }

    /// Delete a secret by its [`VaultRef`].
    ///
    /// # Errors
    /// - [`DaemonError::Capability`] if `secrets.get` is denied.
    /// - [`DaemonError::Vault`] on a vault failure.
    pub fn vault_delete(&self, vault_ref: &VaultRef) -> Result<(), DaemonError> {
        self.authorize(Capability::SecretsGet, &RequestedScope::none())?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        core.vault
            .delete(vault_ref)
            .map_err(|e| DaemonError::Vault(e.to_string()))
    }

    /// A read-only view of the broker's audit log (one entry per authorize call,
    /// allow or deny — DESIGN.md §15.2), cloned out under the core lock.
    #[must_use]
    pub fn audit_log(&self) -> Vec<spork_broker::AuditEntry> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        core.broker.audit_log().to_vec()
    }
}

/// The three borrows `DriftCapture::capture` needs, split out of [`DaemonCore`]
/// so the borrow checker accepts simultaneous `&drift`, `&store`, `&mut graph`.
pub(crate) struct DaemonCoreParts<'a> {
    pub(crate) drift: &'a spork_drift::DriftCapture,
    pub(crate) store: &'a spork_cas::ObjectStore<spork_cas::LooseStore>,
    pub(crate) graph: &'a mut spork_graph::GraphService,
}

impl crate::core::DaemonCore {
    /// Split-borrow the parts `DriftCapture::capture` needs in one call.
    pub(crate) fn drift_capture_parts(&mut self) -> DaemonCoreParts<'_> {
        DaemonCoreParts {
            drift: &self.drift,
            store: &self.store,
            graph: &mut self.graph,
        }
    }
}
