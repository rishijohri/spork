//! Shared descriptor helpers for the built-in node types.
//!
//! The six built-ins all register through the *same* public
//! [`NodeTypeRegistry`](spork_registry::NodeTypeRegistry) path with no
//! special-casing (DESIGN.md §7.1). These small shared constants/helpers keep the
//! descriptors consistent — the snapshot out-port name the contentRef rule keys
//! off (DESIGN.md §7.2), and the initial type version every built-in is published
//! at — without introducing a built-in-only code path.

use semver::Version;

/// The stable name every snapshot-owning built-in uses for its
/// [`PortKind::SnapshotRef`](spork_registry::PortKind::SnapshotRef) out-port.
///
/// The registry's contentRef rule (DESIGN.md §7.2) requires a mutating
/// `owns_snapshot` type to expose an *out* port of that kind; using one shared
/// name keeps the built-ins uniform and a port lookup unambiguous.
pub const SNAPSHOT_OUT_PORT_NAME: &str = "snapshot";

/// The initial semver [`type_version`](spork_registry::NodeTypeDescriptor::type_version)
/// every built-in is first published at.
///
/// A later breaking payload/port change publishes a *new* version alongside this
/// one — the registry retains both, so older nodes still resolve (DESIGN.md
/// §9.2). The descriptor envelope's own schema version is a separate axis
/// ([`DESCRIPTOR_SCHEMA_VERSION`](spork_registry::DESCRIPTOR_SCHEMA_VERSION)).
#[must_use]
pub fn type_version() -> Version {
    Version::new(1, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_type_version_is_1_0_0() {
        assert_eq!(type_version(), Version::new(1, 0, 0));
    }

    #[test]
    fn snapshot_out_port_name_is_stable() {
        assert_eq!(SNAPSHOT_OUT_PORT_NAME, "snapshot");
    }
}
