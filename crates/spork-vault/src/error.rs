//! The vault error type.
//!
//! Realizes the failure surface for the credential vault described in
//! DESIGN.md §15.4. The variants are deliberately coarse: a caller either
//! references a secret that is not present ([`VaultError::NotFound`]) or hits an
//! underlying I/O fault while reading/writing the at-rest store
//! ([`VaultError::Io`]). Crucially, **no error variant ever carries secret
//! material** — `Io` carries only a human-readable description of the failure,
//! so an error value can be logged or surfaced without leaking a credential.

use thiserror::Error;

/// An error returned by a [`CredentialVault`](crate::CredentialVault).
///
/// The variants intentionally never embed secret bytes: an `Io` failure carries
/// a stringified description (path/kind), so vault errors are safe to log and to
/// propagate up through the daemon and IPC layers without exfiltrating a
/// credential (DESIGN.md §15.4).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The referenced secret does not exist in this vault.
    ///
    /// Returned by [`get`](crate::CredentialVault::get) and
    /// [`delete`](crate::CredentialVault::delete) when no entry is keyed by the
    /// supplied [`VaultRef`](crate::VaultRef) (for example, it was already
    /// deleted, or it belongs to a different vault directory).
    #[error("no secret found for the given vault reference")]
    NotFound,

    /// An I/O fault occurred while accessing the at-rest store.
    ///
    /// The payload is a description of the failure only — never secret bytes.
    #[error("vault i/o error: {0}")]
    Io(String),
}

impl From<std::io::Error> for VaultError {
    fn from(e: std::io::Error) -> Self {
        // `NotFound` is modelled explicitly so callers can distinguish a missing
        // reference from a genuine I/O fault; everything else is `Io`.
        if e.kind() == std::io::ErrorKind::NotFound {
            VaultError::NotFound
        } else {
            VaultError::Io(e.to_string())
        }
    }
}
