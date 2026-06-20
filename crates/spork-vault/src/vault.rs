//! The [`CredentialVault`] trait — the seam between Spork and a secret store.
//!
//! `CredentialVault` is the single abstraction over where secret material lives
//! at rest (DESIGN.md §15.4). It deals only in [`Secret`](crate::Secret) values
//! (which cannot be persisted) and opaque [`VaultRef`](crate::VaultRef) handles
//! (which are all that ever land at rest), so a vault implementation is
//! physically incapable of leaking a credential into the content-addressed
//! store: it never sees a serializable secret type.
//!
//! Per constraint C3 ("traits are seams, one real impl now"), this crate ships
//! exactly one backend — [`FileVault`](crate::FileVault). The OS-keychain
//! backend (via the `keyring` crate) is an *additive* implementation deferred to
//! the desktop environment and is not built here.

use crate::{Secret, VaultError, VaultRef};

/// Stores and resolves secret material behind opaque [`VaultRef`] handles.
///
/// Implementations keep secrets out of the content-addressed store and out of
/// git, exposing only the round-trip `put` → [`VaultRef`] → `get`. The trait
/// takes ownership of a [`Secret`] on `put` and returns a fresh `Secret` on
/// `get`; nothing serializable crosses the boundary, so the
/// "secrets never enter the CAS" invariant holds by construction
/// (DESIGN.md §15.4).
///
/// # Object safety
///
/// The trait is object-safe, so a daemon can hold a `Box<dyn CredentialVault>`
/// and swap the `FileVault` for a keychain backend later without touching call
/// sites.
pub trait CredentialVault {
    /// Store `secret` under a human-meaningful `name`, returning the opaque
    /// handle that should be persisted in its place.
    ///
    /// `name` is advisory metadata (e.g. `"anthropic_api_key"`) used for
    /// operator-facing listing/debugging; the returned [`VaultRef`] is what
    /// callers store and later resolve with [`get`](Self::get). Storing two
    /// secrets under the same `name` yields two distinct handles — the vault
    /// keys on the handle, not the name.
    ///
    /// # Errors
    /// Returns [`VaultError::Io`] if the at-rest store cannot be written.
    fn put(&self, name: &str, secret: Secret) -> Result<VaultRef, VaultError>;

    /// Resolve a previously-stored handle back into its [`Secret`].
    ///
    /// # Errors
    /// Returns [`VaultError::NotFound`] if no secret is keyed by `r`, or
    /// [`VaultError::Io`] if the at-rest store cannot be read.
    fn get(&self, r: &VaultRef) -> Result<Secret, VaultError>;

    /// Permanently remove the secret keyed by `r`.
    ///
    /// After a successful `delete`, a subsequent [`get`](Self::get) of the same
    /// handle returns [`VaultError::NotFound`].
    ///
    /// # Errors
    /// Returns [`VaultError::NotFound`] if no secret is keyed by `r`, or
    /// [`VaultError::Io`] if the at-rest store cannot be mutated.
    fn delete(&self, r: &VaultRef) -> Result<(), VaultError>;
}
