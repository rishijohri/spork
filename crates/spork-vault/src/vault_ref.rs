//! The opaque [`VaultRef`] handle — what Spork stores in place of a secret.
//!
//! A [`VaultRef`] is the persistable counterpart to a [`Secret`](crate::Secret).
//! Everywhere a credential is *referenced* — a node payload, a model-provider
//! config, an audit entry — Spork holds a `VaultRef`, never the bytes. The
//! handle is an opaque, random identifier with no relationship to the secret it
//! points at: it can safely be serialized into the content-addressed store,
//! exported as part of a handoff document, and logged, because possessing the
//! handle does not reveal the secret. Resolution happens only inside the daemon,
//! against a live [`CredentialVault`](crate::CredentialVault) (DESIGN.md §15.4).
//!
//! `VaultRef` derives `Serialize`/`Deserialize` *on purpose* — it is the one
//! part of the secrets model that is meant to land at rest.

use serde::{Deserialize, Serialize};

/// The current schema version of the [`VaultRef`] wire/disk format.
///
/// `VaultRef` is a single newtype string, but it still carries a version
/// constant so the persisted handle format can evolve additively without a flag
/// day (constraint C5; DESIGN.md A.7). The version is embedded in the handle's
/// textual form via the [`VaultRef::PREFIX`].
pub const VAULT_REF_SCHEMA_VERSION: u16 = 1;

/// An opaque reference to a secret held in a [`CredentialVault`](crate::CredentialVault).
///
/// A `VaultRef` is **not** the secret and reveals nothing about it: it is a
/// random, versioned identifier (`"vref.1:<hex>"`). It is the value that gets
/// stored, referenced, exported, and logged in place of the credential. To turn
/// a `VaultRef` back into a [`Secret`](crate::Secret) you must call
/// [`get`](crate::CredentialVault::get) against the live vault that minted it.
///
/// Unlike [`Secret`](crate::Secret), `VaultRef` derives `Serialize`/
/// `Deserialize`, because persisting the *handle* (never the bytes) is exactly
/// the intended behavior (DESIGN.md §15.4).
///
/// # Example
/// ```
/// use spork_vault::VaultRef;
///
/// let r = VaultRef::generate();
/// // Handles are opaque and carry the versioned prefix.
/// assert!(r.as_str().starts_with("vref.1:"));
///
/// // Two freshly-generated handles are distinct.
/// assert_ne!(VaultRef::generate(), VaultRef::generate());
///
/// // Handles round-trip through their string form.
/// let s = r.as_str().to_owned();
/// assert_eq!(VaultRef::from_string(s), r);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VaultRef(pub String);

impl VaultRef {
    /// The textual prefix every generated handle carries: `"vref.1:"`.
    ///
    /// The embedded `1` is [`VAULT_REF_SCHEMA_VERSION`]; a future handle format
    /// would mint a higher generation while old handles keep theirs (the
    /// no-domino seam, DESIGN.md A.7).
    pub const PREFIX: &'static str = "vref.1:";

    /// Mint a fresh, random, opaque handle.
    ///
    /// The identifier is unguessable and bears no relation to any secret. It is
    /// produced from 128 bits of cryptographically-secure OS randomness (the OS
    /// CSPRNG via `getrandom`) rendered as hex behind the versioned
    /// [`PREFIX`](Self::PREFIX).
    #[must_use]
    pub fn generate() -> Self {
        VaultRef(format!("{}{}", Self::PREFIX, random_hex_128()))
    }

    /// Wrap an existing handle string (e.g. one read back from storage).
    #[must_use]
    pub fn from_string(s: String) -> Self {
        VaultRef(s)
    }

    /// Borrow the handle's textual form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The schema version this crate stamps onto freshly-generated handles.
    #[must_use]
    pub fn schema_version() -> u16 {
        VAULT_REF_SCHEMA_VERSION
    }
}

impl std::fmt::Display for VaultRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Produce 128 bits of cryptographically-secure randomness as a 32-char
/// lowercase hex string.
///
/// Bytes are filled directly from the operating system CSPRNG via `getrandom`
/// (e.g. `getentropy`/`/dev/urandom` on Unix). Handles are opaque, unguessable,
/// and bear no relation to any secret; they are never used as cryptographic keys.
fn random_hex_128() -> String {
    use std::fmt::Write as _;

    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG (getrandom) must be available to mint VaultRefs");

    let mut hex = String::with_capacity(32);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_handles_carry_versioned_prefix() {
        let r = VaultRef::generate();
        assert!(r.as_str().starts_with(VaultRef::PREFIX));
        assert!(r.as_str().starts_with("vref.1:"));
        assert_eq!(VaultRef::schema_version(), 1);
    }

    #[test]
    fn generated_handles_are_unique() {
        // 10k handles with zero collisions exercises the entropy mixing.
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(VaultRef::generate()));
        }
    }

    #[test]
    fn handles_are_distinct_across_threads() {
        let mut handles = Vec::new();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| (0..1000).map(|_| VaultRef::generate()).collect::<Vec<_>>())
            })
            .collect();
        for t in threads {
            handles.extend(t.join().unwrap());
        }
        let unique: HashSet<_> = handles.iter().collect();
        assert_eq!(unique.len(), handles.len());
    }

    #[test]
    fn string_roundtrip() {
        let r = VaultRef::generate();
        let s = r.as_str().to_owned();
        assert_eq!(VaultRef::from_string(s), r);
    }

    #[test]
    fn serde_roundtrip() {
        let r = VaultRef::generate();
        let json = serde_json::to_string(&r).unwrap();
        let back: VaultRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn display_matches_as_str() {
        let r = VaultRef::generate();
        assert_eq!(r.to_string(), r.as_str());
    }
}
