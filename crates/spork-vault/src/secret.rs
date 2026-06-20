//! The zeroizing [`Secret`] — secret material that scrubs itself and refuses to
//! be persisted.
//!
//! A [`Secret`] is the only place raw credential bytes ever live in Spork. It
//! deliberately upholds two properties that, together, make it impossible for a
//! secret to leak into a snapshot, the content-addressed store, or git
//! (DESIGN.md §15.4, §11.3, A.6):
//!
//! 1. **Zeroized on drop.** The bytes are held in a [`Zeroizing<Vec<u8>>`], so
//!    when the `Secret` is dropped its buffer is overwritten with zeros rather
//!    than merely freed. A secret resolved at runtime does not linger in
//!    reclaimed heap memory.
//! 2. **Not serializable, not hashable, not printable.** `Secret` does **not**
//!    implement `serde::Serialize`, `Display`, or a revealing `Debug`. Because
//!    every Spork object reaches the CAS through `serde` canonical encoding, a
//!    type that cannot be serialized **cannot enter the CAS at all** — the
//!    compiler enforces the invariant. Its `Debug` redacts the contents so a
//!    secret cannot leak through a log line or a panic message.
//!
//! Reading the bytes back is intentionally awkward: it requires the explicit
//! [`Secret::expose`] accessor, which makes every read site greppable and
//! auditable.

use zeroize::Zeroizing;

/// Secret credential material, zeroized on drop and impossible to persist.
///
/// `Secret` is the runtime-only carrier for a credential (an API key, token, or
/// PEM private key). It is the deliberate counterpart to the opaque
/// [`VaultRef`](crate::VaultRef): a `VaultRef` is what gets *stored and
/// referenced*, while a `Secret` is what is *resolved at execution time and then
/// scrubbed*.
///
/// # Why it cannot be persisted
///
/// `Secret` intentionally implements **none** of `serde::Serialize`,
/// `Clone`-into-bytes-friendly conversions, `Display`, `AsRef<[u8]>`, or a
/// revealing `Debug`. Spork only ever writes bytes into the content-addressed
/// store through canonical `serde` encoding, so a value that is not
/// `Serialize` simply has no path into a `spork-cas` blob, tree, or snapshot.
/// This turns "secrets never enter the CAS" from a convention into a
/// **compile-time guarantee** (DESIGN.md §15.4, §11.3).
///
/// # Reading the bytes
///
/// The raw bytes are reachable only through [`Secret::expose`]. There is no
/// implicit deref or `AsRef`, so every place that touches plaintext is explicit
/// and greppable.
///
/// # Example
/// ```
/// use spork_vault::Secret;
///
/// let s = Secret::new(b"super-secret-token".to_vec());
/// assert_eq!(s.len(), 18);
/// assert_eq!(s.expose(), b"super-secret-token");
///
/// // Debug never reveals the contents.
/// assert_eq!(format!("{s:?}"), "Secret(<redacted; 18 bytes>)");
/// ```
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    /// Wrap raw bytes as a [`Secret`].
    ///
    /// Takes ownership of `bytes`; the buffer is zeroized when the `Secret` is
    /// dropped. Prefer handing ownership of a freshly-read buffer here so the
    /// plaintext exists in exactly one place.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Secret(Zeroizing::new(bytes))
    }

    /// Borrow the raw secret bytes.
    ///
    /// This is the **only** way to read the plaintext. It is named to be
    /// conspicuous so that audit greps for `.expose(` find every plaintext use
    /// site. The returned slice borrows the `Secret`; it must not be copied into
    /// any long-lived or serializable structure.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// The length of the secret in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the secret is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<u8>> for Secret {
    fn from(bytes: Vec<u8>) -> Self {
        Secret::new(bytes)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Secret::new(s.as_bytes().to_vec())
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        // Convert through bytes; the temporary `String`'s buffer is dropped, but
        // the resulting `Secret` owns the only retained copy and zeroizes it.
        Secret::new(s.into_bytes())
    }
}

/// A redacting `Debug` so a `Secret` can appear in logs or panic messages
/// without ever revealing its contents — only its length is shown.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<redacted; {} bytes>)", self.0.len())
    }
}

/// Two secrets compare equal iff their bytes are equal.
///
/// This is provided for tests and for de-duplication checks; it is **not** a
/// constant-time comparison and must not be used to gate authentication.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Secret {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_bytes() {
        let s = Secret::new(b"abc123".to_vec());
        assert_eq!(s.expose(), b"abc123");
        assert_eq!(s.len(), 6);
        assert!(!s.is_empty());
    }

    #[test]
    fn empty_secret() {
        let s = Secret::new(Vec::new());
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.expose(), b"");
    }

    #[test]
    fn debug_is_redacted() {
        let s = Secret::new(b"top-secret".to_vec());
        let rendered = format!("{s:?}");
        assert_eq!(rendered, "Secret(<redacted; 10 bytes>)");
        // The plaintext must never appear in the Debug output.
        assert!(!rendered.contains("top-secret"));
    }

    #[test]
    fn from_str_and_string() {
        let a: Secret = "hunter2".into();
        let b: Secret = String::from("hunter2").into();
        let c: Secret = "hunter2".as_bytes().to_vec().into();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a.expose(), b"hunter2");
    }

    #[test]
    fn equality_is_by_content() {
        assert_eq!(Secret::new(b"k".to_vec()), Secret::new(b"k".to_vec()));
        assert_ne!(Secret::new(b"k".to_vec()), Secret::new(b"K".to_vec()));
    }
}
