//! The IPC error contract — the single failure type every [`CommandHandler`]
//! dispatch returns.
//!
//! [`IpcError`] is the renderer-facing failure vocabulary. It is deliberately
//! coarse: each variant names a *domain boundary* (capability check, graph
//! mutation, restore guard, git coexistence, lookup) rather than leaking the
//! internal error type of the daemon subsystem that produced it. The daemon
//! collapses its rich internal errors (`spork-broker`, `spork-graph`,
//! `spork-restore`, `spork-git`) into one of these variants with a human string,
//! so the frozen IPC surface never grows a dependency on a daemon-internal type.
//!
//! The enum is `#[non_exhaustive]` so a future daemon can introduce a new
//! failure domain (a new variant) without breaking renderers that match on it —
//! they keep their `_ =>` arm. This is the additive-evolution discipline of
//! CLAUDE.md C2/C5 applied to the error half of the contract.
//!
//! Design references: DESIGN.md §5.5 (the daemon is the security boundary; every
//! privileged call is capability-gated and can be denied), §14.1 (the typed IPC
//! surface), A.1 (the command contract these errors accompany).
//!
//! [`CommandHandler`]: crate::CommandHandler

use thiserror::Error;

/// The error returned by [`CommandHandler::dispatch`](crate::CommandHandler::dispatch).
///
/// Each variant is a *domain*, not a specific cause: the contained `String` is a
/// human-readable detail the daemon fills in from the failing subsystem. The
/// renderer pattern-matches on the variant to choose a recovery path (re-prompt
/// for a capability, show a divergence dialog, surface a not-found state) and
/// shows the string as context.
///
/// `#[non_exhaustive]`: new failure domains may be appended over time, so all
/// downstream matches must include a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IpcError {
    /// A privileged operation was refused by the capability broker
    /// (DESIGN.md §15.1–§15.2). The string carries the denied capability and
    /// the reason. This is the *expected* outcome of a deny-by-default check,
    /// not a bug — the renderer typically responds by requesting a grant.
    #[error("capability denied: {0}")]
    Capability(String),

    /// A graph mutation was rejected by the graph service: a registry-contract
    /// violation, an acyclicity violation, a duplicate id, or an append failure
    /// (DESIGN.md §6.2–§6.3).
    #[error("graph error: {0}")]
    Graph(String),

    /// The atomic dual-restore guard failed and rolled back, changing nothing
    /// (DESIGN.md §6.4, §10.3, §11.4). The string explains the divergence (a
    /// missing or mismatched snapshot/conversation ref).
    #[error("restore error: {0}")]
    Restore(String),

    /// A non-invasive git operation failed (DESIGN.md §10.4): the path is not a
    /// repository, or libgit2/`git` reported an error. By contract this never
    /// implies the user's `.git`/index/working-tree was mutated.
    #[error("git error: {0}")]
    Git(String),

    /// A referenced entity (node, ref, blob path, or op id) does not exist. The
    /// string names what was looked up.
    #[error("not found: {0}")]
    NotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_domain_and_detail() {
        assert_eq!(
            IpcError::Capability("ProcessSpawn denied".into()).to_string(),
            "capability denied: ProcessSpawn denied"
        );
        assert_eq!(
            IpcError::Graph("cycle".into()).to_string(),
            "graph error: cycle"
        );
        assert_eq!(
            IpcError::Restore("divergence".into()).to_string(),
            "restore error: divergence"
        );
        assert_eq!(
            IpcError::Git("not a repo".into()).to_string(),
            "git error: not a repo"
        );
        assert_eq!(
            IpcError::NotFound("node 7".into()).to_string(),
            "not found: node 7"
        );
    }

    #[test]
    fn is_cloneable_and_comparable() {
        let e = IpcError::Restore("x".into());
        assert_eq!(e.clone(), e);
    }
}
