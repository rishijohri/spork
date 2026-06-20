//! The daemon's internal error type and its lowering to the frozen
//! [`spork_ipc::IpcError`] surface.
//!
//! [`DaemonError`] is the daemon's *internal* failure vocabulary — it names the
//! subsystem that failed (graph, CAS, vault, restore, git, drift, broker, log,
//! io) with a human string. The renderer never sees it: every
//! [`CommandHandler::dispatch`](spork_ipc::CommandHandler::dispatch) collapses it
//! into one of the coarse [`spork_ipc::IpcError`] domains via [`DaemonError::into`]
//! (a `From` impl), so the frozen IPC contract never grows a dependency on a
//! daemon-internal type (DESIGN.md §5.5, §14.1).
//!
//! The mapping is deliberate: a broker denial becomes
//! [`IpcError::Capability`](spork_ipc::IpcError::Capability) (the *expected*
//! deny-by-default outcome, not a bug); a restore divergence becomes
//! [`IpcError::Restore`](spork_ipc::IpcError::Restore) (fail-closed, nothing
//! changed); a git failure becomes [`IpcError::Git`](spork_ipc::IpcError::Git)
//! (by contract `.git` is never mutated improperly); a missing entity becomes
//! [`IpcError::NotFound`](spork_ipc::IpcError::NotFound); everything else folds
//! into [`IpcError::Graph`](spork_ipc::IpcError::Graph).

use spork_ipc::IpcError;
use thiserror::Error;

/// The daemon's internal error.
///
/// `#[non_exhaustive]` so a future subsystem can add a domain without breaking a
/// downstream match (CLAUDE.md C2).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DaemonError {
    /// A capability was denied by the broker (deny-by-default). Carries the
    /// broker's reason; the audit log already recorded the denial.
    #[error("capability denied: {0}")]
    Capability(String),

    /// A graph mutation/read failed in the F2 service (registry contract,
    /// acyclicity, append).
    #[error("graph error: {0}")]
    Graph(String),

    /// A content-store operation failed (capture, read, materialize).
    #[error("cas error: {0}")]
    Cas(String),

    /// The credential vault failed (put/get/delete).
    #[error("vault error: {0}")]
    Vault(String),

    /// The atomic dual-restore guard failed and rolled back, changing nothing.
    #[error("restore error: {0}")]
    Restore(String),

    /// A non-invasive git operation failed.
    #[error("git error: {0}")]
    Git(String),

    /// The drift-capture pipeline failed.
    #[error("drift error: {0}")]
    Drift(String),

    /// The F1 event log / writer / reader failed.
    #[error("log error: {0}")]
    Log(String),

    /// A referenced entity (node, ref, blob path, op) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A filesystem operation outside a typed subsystem failed.
    #[error("io error: {0}")]
    Io(String),
}

impl From<DaemonError> for IpcError {
    /// Collapse a rich internal error into the coarse, frozen IPC domain the
    /// renderer matches on. See the module docs for the rationale of each arm.
    fn from(e: DaemonError) -> Self {
        match e {
            DaemonError::Capability(s) => IpcError::Capability(s),
            DaemonError::Restore(s) => IpcError::Restore(s),
            DaemonError::Git(s) => IpcError::Git(s),
            DaemonError::NotFound(s) => IpcError::NotFound(s),
            // Graph/CAS/vault/drift/log/io are all daemon-internal failure
            // domains the renderer treats as a generic graph-side error.
            DaemonError::Graph(s)
            | DaemonError::Cas(s)
            | DaemonError::Vault(s)
            | DaemonError::Drift(s)
            | DaemonError::Log(s)
            | DaemonError::Io(s) => IpcError::Graph(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_lowers_to_capability_domain() {
        let ipc: IpcError = DaemonError::Capability("ProcessSpawn denied".into()).into();
        assert!(matches!(ipc, IpcError::Capability(_)));
    }

    #[test]
    fn restore_lowers_to_restore_domain() {
        let ipc: IpcError = DaemonError::Restore("divergence".into()).into();
        assert!(matches!(ipc, IpcError::Restore(_)));
    }

    #[test]
    fn git_lowers_to_git_domain() {
        let ipc: IpcError = DaemonError::Git("not a repo".into()).into();
        assert!(matches!(ipc, IpcError::Git(_)));
    }

    #[test]
    fn not_found_lowers_to_not_found_domain() {
        let ipc: IpcError = DaemonError::NotFound("node 7".into()).into();
        assert!(matches!(ipc, IpcError::NotFound(_)));
    }

    #[test]
    fn internal_domains_fold_to_graph() {
        for e in [
            DaemonError::Graph("g".into()),
            DaemonError::Cas("c".into()),
            DaemonError::Vault("v".into()),
            DaemonError::Drift("d".into()),
            DaemonError::Log("l".into()),
            DaemonError::Io("i".into()),
        ] {
            let ipc: IpcError = e.into();
            assert!(matches!(ipc, IpcError::Graph(_)));
        }
    }
}
