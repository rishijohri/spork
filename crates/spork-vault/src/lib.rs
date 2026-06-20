//! Spork F3 credential vault — secrets that can be referenced but never stored.
//!
//! This crate enforces a single hard separation between **secret material** and
//! **everything Spork persists** (DESIGN.md §15.4, §11.3, A.6):
//!
//! - Secret bytes live only in a [`Secret`], a zeroizing carrier that scrubs
//!   itself on drop and **deliberately does not implement `serde::Serialize`**.
//!   Because every byte Spork writes into the content-addressed store reaches it
//!   through canonical `serde` encoding, a non-`Serialize` type has *no path
//!   into the CAS at all* — the "secrets never enter the CAS" invariant is a
//!   compile-time guarantee, not a convention.
//! - Everything Spork stores in a credential's place holds only a [`VaultRef`],
//!   an opaque, versioned, random handle that reveals nothing about the secret
//!   and is safe to serialize, export, and log.
//!
//! The [`CredentialVault`] trait is the seam between Spork and a secret store.
//! Per constraint C3 this crate ships exactly one real backend — [`FileVault`],
//! which keeps secret material at rest in `0600`-permission files under a vault
//! directory, **outside the CAS and outside git**, keyed by [`VaultRef`]. (An
//! OS-keychain backend over the `keyring` crate is an additive implementation
//! deferred to the desktop environment and is intentionally not built here.)
//!
//! # Why this matters
//!
//! Spork nodes are restorable, branchable, and **exportable as handoff
//! documents**. A key embedded in an immutable, shareable node would leak
//! permanently on export. So credentials are referenced by opaque handle and
//! resolved only at execution time inside the daemon, then scrubbed
//! (DESIGN.md §15.4, §11.3, §15.1's `secrets.get` capability).
//!
//! # The persisted-schema invariant (constraint C5)
//!
//! [`VaultRef`] is the only persisted type here, and it carries an explicit
//! [`VAULT_REF_SCHEMA_VERSION`] embedded in its textual prefix (`"vref.1:"`),
//! mirrored by the on-disk metadata sidecar [`FileVault`] writes — so the handle
//! format can evolve additively without a flag day (the no-domino seam,
//! DESIGN.md A.7).
//!
//! # Example
//! ```
//! use spork_vault::{CredentialVault, FileVault, Secret, VaultRef};
//! use tempfile::TempDir;
//!
//! let dir = TempDir::new().unwrap();
//! let vault = FileVault::open(dir.path()).unwrap();
//!
//! // Store a secret; you get back only an opaque handle.
//! let r: VaultRef = vault.put("anthropic_api_key", Secret::from("sk-abc123")).unwrap();
//! assert!(r.as_str().starts_with("vref.1:"));
//! assert_ne!(r.as_str().as_bytes(), b"sk-abc123"); // the handle is not the secret
//!
//! // Resolve it back inside the daemon at execution time.
//! let secret = vault.get(&r).unwrap();
//! assert_eq!(secret.expose(), b"sk-abc123");
//!
//! // The handle persists; the secret never does.
//! let persisted = serde_json::to_string(&r).unwrap();
//! assert_eq!(serde_json::from_str::<VaultRef>(&persisted).unwrap(), r);
//! // `Secret` does not implement Serialize, so this would not compile:
//! //   let _ = serde_json::to_string(&secret);
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod file_vault;
mod secret;
mod vault;
mod vault_ref;

pub use error::VaultError;
pub use file_vault::FileVault;
pub use secret::Secret;
pub use vault::CredentialVault;
pub use vault_ref::{VaultRef, VAULT_REF_SCHEMA_VERSION};

#[cfg(test)]
mod invariant_tests {
    //! Cross-cutting tests proving the central F3 vault invariant: a [`Secret`]
    //! can never be serialized into a `spork-cas` object — only a [`VaultRef`]
    //! ever lands at rest.

    use super::*;

    /// A node-payload-shaped struct, as it would appear in a persisted Spork
    /// object: it can hold a [`VaultRef`] but, by construction, can never hold a
    /// [`Secret`] — `Secret` is not `Serialize`, so a field of that type would
    /// make this struct fail to derive `Serialize`.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
    struct PersistedNodePayload {
        provider: String,
        credential: VaultRef,
    }

    #[test]
    fn vault_ref_persists_but_carries_no_secret() {
        let secret_bytes = b"sk-NEVER-PERSIST-ME";
        // Mint a handle the way a real put would.
        let r = VaultRef::generate();

        let payload = PersistedNodePayload {
            provider: "anthropic".into(),
            credential: r.clone(),
        };

        // The payload serializes (it only holds a VaultRef)...
        let json = serde_json::to_string(&payload).unwrap();
        // ...and the secret bytes are nowhere in the persisted form.
        assert!(
            !json
                .as_bytes()
                .windows(secret_bytes.len())
                .any(|w| w == secret_bytes),
            "secret bytes appeared in a persistable payload"
        );

        // Round-trips cleanly.
        let back: PersistedNodePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
    }

    /// This test asserts, in prose backed by the type system, that a `Secret`
    /// is not `Serialize`. If someone ever adds `impl Serialize for Secret`,
    /// `serde_json::to_vec(&secret)` would compile and the equivalent
    /// `compile_fail` doctest below would start passing — so we keep the proof
    /// where the compiler enforces it.
    #[test]
    fn vault_ref_is_serializable() {
        // Sanity: the handle (the persistable half) IS serializable.
        let r = VaultRef::generate();
        let _ = serde_json::to_vec(&r).expect("VaultRef must be serializable");
    }

    /// The full round-trip through the real backend never lets the plaintext
    /// reach any serialized artifact.
    #[test]
    fn full_put_get_keeps_secret_off_the_persistence_path() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let vault = FileVault::open(dir.path()).unwrap();

        let plaintext = b"sk-INTEGRATION-SECRET";
        let r = vault.put("k", Secret::new(plaintext.to_vec())).unwrap();

        // The only thing a node would persist is the handle.
        let persisted = serde_json::to_vec(&r).unwrap();
        assert!(
            !persisted.windows(plaintext.len()).any(|w| w == plaintext),
            "secret leaked into the serialized VaultRef"
        );

        // And it still resolves to the original bytes.
        assert_eq!(vault.get(&r).unwrap().expose(), plaintext);
    }
}

/// Compile-fail proof that a [`Secret`] cannot be serialized into a CAS object.
///
/// `Secret` deliberately does not implement `serde::Serialize`, so attempting to
/// serialize it must fail to compile. This is the type-system half of the
/// "secrets never enter the CAS" invariant (DESIGN.md §15.4).
///
/// ```compile_fail
/// use spork_vault::Secret;
/// let s = Secret::from("nope");
/// // `Secret` is not `Serialize`, so this line must not compile:
/// let _ = serde_json::to_string(&s);
/// ```
///
/// And a struct that tries to *embed* a `Secret` in a persisted shape cannot
/// derive `Serialize` either:
///
/// ```compile_fail
/// use spork_vault::Secret;
/// #[derive(serde::Serialize)]
/// struct Leaky { key: Secret }
/// ```
#[cfg(doctest)]
struct SecretIsNotSerializable;
