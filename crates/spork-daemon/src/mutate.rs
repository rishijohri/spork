//! Mutation helpers: ordered-event publication, the transient restore guard, and
//! the live-graph refresh after a guarded op.
//!
//! Every mutation the daemon performs appends a durable F1 event through the
//! single-writer actor (via the [`GraphService`](spork_graph::GraphService) or
//! the [`RestoreGuard`](spork_restore::RestoreGuard)) and then publishes the
//! corresponding renderer-facing [`OpLogEvent`] onto the ordered rail. The rule
//! the whole IPC contract rests on is realized here: the *mutation return value*
//! is only an `op_id`; the resulting state flows out **exclusively** on the
//! ordered [`EventStream`](spork_stream::EventStream) (DESIGN.md §5.5, §14.4).
//!
//! Restore and branch-fork go through `spork-restore`'s atomic guard. Because
//! the guard *owns* a `GraphService`, the daemon hands it a transient service
//! rebuilt from the shared log, runs the guarded op (which appends to the same
//! single-writer log), then refreshes the daemon's own live service from the log
//! — the graph is a pure projection, so the rebuild is exact (DESIGN.md §6.4,
//! §10.3, and the F2 projection-soundness property).

use spork_cas::{LooseStore, ObjectStore};
use spork_ipc::OpLogEvent;
use spork_restore::{RefId, RestoreGuard, RestoreOutcome};
use ulid::Ulid;

use crate::core::{build_graph_service, Daemon};
use crate::error::DaemonError;

impl Daemon {
    /// Publish one renderer-facing event onto the ordered rail with the next
    /// dense `seq`.
    ///
    /// The daemon owns the rail's contiguous `seq` counter (see
    /// [`crate::core`]); this stamps the event with the next value and fans it
    /// out. A [`spork_stream::StreamError::SeqGap`] is impossible by construction
    /// (we always pass the expected next `seq`) but is surfaced as a
    /// [`DaemonError::Log`] rather than silently ignored.
    pub(crate) fn publish_event(
        &self,
        make: impl FnOnce(u64) -> OpLogEvent,
    ) -> Result<(), DaemonError> {
        let mut next = self.next_seq.lock().expect("daemon seq lock poisoned");
        let event = make(*next);
        self.events
            .publish(event)
            .map_err(|e| DaemonError::Log(format!("event publish: {e}")))?;
        *next += 1;
        Ok(())
    }

    /// Run the atomic dual-restore through a transient [`RestoreGuard`], then
    /// refresh the daemon's live graph from the shared log.
    ///
    /// The guard is constructed over a freshly rebuilt graph service and a
    /// reopened store (both sharing the daemon's single-writer log and CAS), so
    /// the guarded op is serialized through the same write path as every other
    /// mutation. The whole operation — build guard, run it, refresh the live
    /// service — runs while **holding the core lock**, so a restore is a single
    /// serialized critical section against every other dispatch (no two restores
    /// can race on the working-directory swap, honoring the §10.3 single-lock
    /// atomicity contract). On success the daemon's own live service is rebuilt
    /// from the log (the guard's `ref_moved`/`restore.performed` appends are now
    /// visible), keeping the view-model surface consistent.
    ///
    /// On failure the guard fails closed (nothing changed); the live service is
    /// left as-is because the log was not advanced.
    pub(crate) fn guarded_restore(&self, node_id: Ulid) -> Result<RestoreOutcome, DaemonError> {
        let mut core = self.core.lock().expect("daemon core mutex poisoned");
        let workdir = core.workdir.clone();
        let guard = self.build_restore_guard(workdir)?;
        let outcome = guard
            .restore(node_id)
            .map_err(|e| DaemonError::Restore(e.to_string()))?;
        // Rebuild the live service in place while still holding the lock, so the
        // restore's effects become visible atomically with the op completing.
        core.graph = build_graph_service(&self.log, &self.writer)?;
        Ok(outcome)
    }

    /// Create a new branch ref at a node through the guard — metadata only,
    /// zero bytes copied — then refresh the live graph.
    ///
    /// Like [`guarded_restore`](Self::guarded_restore), the whole operation runs
    /// while holding the core lock so it is serialized against every other
    /// dispatch.
    pub(crate) fn guarded_branch_fork(
        &self,
        from_node_id: Ulid,
        name: &str,
    ) -> Result<RefId, DaemonError> {
        let mut core = self.core.lock().expect("daemon core mutex poisoned");
        let workdir = core.workdir.clone();
        let guard = self.build_restore_guard(workdir)?;
        let ref_id = guard
            .branch_fork(from_node_id, name)
            .map_err(|e| DaemonError::Restore(e.to_string()))?;
        core.graph = build_graph_service(&self.log, &self.writer)?;
        Ok(ref_id)
    }

    /// Build a transient restore guard over a rebuilt service + reopened store.
    fn build_restore_guard(
        &self,
        workdir: std::path::PathBuf,
    ) -> Result<RestoreGuard<LooseStore>, DaemonError> {
        let graph = build_graph_service(&self.log, &self.writer)?;
        let store = ObjectStore::new(
            LooseStore::open(&self.cas_dir).map_err(|e| DaemonError::Cas(e.to_string()))?,
        );
        Ok(RestoreGuard::new(
            graph,
            store,
            workdir,
            self.writer.clone(),
        ))
    }
}
