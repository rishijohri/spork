//! The frozen capability vocabulary: [`Capability`].
//!
//! A capability is the *kind* of side effect an executor (built-in or
//! user-defined node-type runner) may attempt. Spork's security model is that
//! executors get **no ambient authority** — every privileged operation is named
//! by one of a small, fixed, typed vocabulary, and the only path to performing
//! it is through the [`CapabilityBroker`](crate::CapabilityBroker), which mints a
//! short-lived scoped token (DESIGN.md §15.2).
//!
//! # Why the set is frozen
//!
//! The capability vocabulary is the contract between manifests (which *declare*
//! the capabilities a runner needs), the user (who *grants* them at install),
//! and the broker (which *enforces* them). That contract must be stable: a
//! manifest that asks for `net.connect` today must mean exactly the same thing
//! to a broker built years later. So [`Capability`] is a closed, ordered value
//! enum — *not* `#[non_exhaustive]` — and the variant set is the frozen
//! vocabulary from the DESIGN.md §15.2 capability table. New side-effect classes
//! are out of scope for this contract by design; the seams that *do* evolve are
//! the [`Scope`](crate::Scope) grammar and the [`AuditEntry`](crate::AuditEntry)
//! record, which each carry a `schema_version`.
//!
//! # Wire form
//!
//! Each capability serializes to the stable dotted string used in manifests and
//! the design's capability table (`"snapshot.read"`, `"net.connect"`, …) rather
//! than the Rust variant name, so persisted grants and audit entries read the
//! same as the manifests that requested them.
//!
//! Design references: DESIGN.md §15.1 (trust boundaries — all privileged
//! operations live behind the broker), §15.2 (the capability table this enum
//! enumerates).

use serde::{Deserialize, Serialize};

/// The schema version of the [`Capability`] vocabulary itself.
///
/// The vocabulary is frozen, so this is `1`. It exists so that the (deliberately
/// rare) event of the *frozen set* being revised — which would be a major,
/// coordinated change — is still self-describing in any persisted form
/// (CLAUDE.md C5).
pub const CAPABILITY_SCHEMA_VERSION: u16 = 1;

/// The fixed, frozen set of side-effect classes the broker arbitrates.
///
/// Each variant corresponds to one row of the DESIGN.md §15.2 capability table.
/// The set is closed on purpose (see the module docs): it is the stable contract
/// manifests declare against and the broker enforces.
///
/// Serialization uses the dotted manifest spelling (e.g. `"snapshot.read"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read bytes from snapshots, restricted by path globs (CAS broker).
    #[serde(rename = "snapshot.read")]
    SnapshotRead,
    /// Write bytes into a copy-on-write working copy, restricted by path globs
    /// (CAS broker). Writes never touch the user's real working tree.
    #[serde(rename = "snapshot.write")]
    SnapshotWrite,
    /// Spawn a subprocess (only ever inside a sandbox/container tier).
    #[serde(rename = "process.spawn")]
    ProcessSpawn,
    /// Open an outbound network connection, restricted by a host allowlist
    /// (broker egress filter).
    #[serde(rename = "net.connect")]
    NetConnect,
    /// Invoke a model, bounded by a per-node token/USD budget (model broker).
    #[serde(rename = "model.invoke")]
    ModelInvoke,
    /// Read another node's outputs, restricted to a lineage-only, node-type
    /// allowlist (graph engine).
    #[serde(rename = "nodes.readOutputs")]
    NodesReadOutputs,
    /// Fetch a named secret handle from the credential vault (CredentialVault).
    #[serde(rename = "secrets.get")]
    SecretsGet,
}

impl Capability {
    /// Every capability in the frozen vocabulary, in declaration order.
    ///
    /// Useful for exhaustive iteration (audits, UI listings, property tests)
    /// without re-listing the variants at each call site. Because the set is
    /// frozen, this slice is the complete vocabulary.
    pub const ALL: [Capability; 7] = [
        Capability::SnapshotRead,
        Capability::SnapshotWrite,
        Capability::ProcessSpawn,
        Capability::NetConnect,
        Capability::ModelInvoke,
        Capability::NodesReadOutputs,
        Capability::SecretsGet,
    ];

    /// The stable dotted manifest spelling of this capability.
    ///
    /// This matches the serde wire form and the DESIGN.md §15.2 table spelling
    /// (e.g. [`Capability::SnapshotRead`] → `"snapshot.read"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::SnapshotRead => "snapshot.read",
            Capability::SnapshotWrite => "snapshot.write",
            Capability::ProcessSpawn => "process.spawn",
            Capability::NetConnect => "net.connect",
            Capability::ModelInvoke => "model.invoke",
            Capability::NodesReadOutputs => "nodes.readOutputs",
            Capability::SecretsGet => "secrets.get",
        }
    }

    /// Whether this capability's scope is constrained by **path globs**.
    ///
    /// Snapshot read/write are path-scoped (DESIGN.md §15.2); other capabilities
    /// ignore the path-glob dimension of a [`Scope`](crate::Scope).
    #[must_use]
    pub const fn is_path_scoped(self) -> bool {
        matches!(self, Capability::SnapshotRead | Capability::SnapshotWrite)
    }

    /// Whether this capability's scope is constrained by a **host allowlist**.
    ///
    /// Only [`Capability::NetConnect`] is host-scoped (DESIGN.md §15.2 broker
    /// egress filter).
    #[must_use]
    pub const fn is_host_scoped(self) -> bool {
        matches!(self, Capability::NetConnect)
    }

    /// Whether this capability's scope is constrained by a **token/USD budget**.
    ///
    /// Only [`Capability::ModelInvoke`] is budget-scoped (DESIGN.md §15.2 model
    /// broker, per-node token/USD budget).
    #[must_use]
    pub const fn is_budget_scoped(self) -> bool {
        matches!(self, Capability::ModelInvoke)
    }
}
