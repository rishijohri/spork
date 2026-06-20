//! [`FileVault`] — the one real [`CredentialVault`](crate::CredentialVault)
//! backend in F3.
//!
//! `FileVault` keeps secret material at rest in a per-handle file under a vault
//! directory, **outside the content-addressed store and outside git**, with
//! owner-only (`0600`) permissions on Unix and an owner-only directory
//! (`0700`). It is the headless, testable counterpart to the OS-keychain backend
//! deferred to the desktop environment (DESIGN.md §15.4).
//!
//! # On-disk layout
//!
//! ```text
//! <dir>/                       (0700)
//!   vref.1:<hex>.secret        (0600)  raw secret bytes
//!   vref.1:<hex>.meta          (0600)  JSON sidecar: { schema_version, name }
//! ```
//!
//! The secret file holds nothing but the raw bytes; the `.meta` sidecar is the
//! only place the advisory `name` and the [`schema version`](crate::VAULT_REF_SCHEMA_VERSION)
//! are recorded (constraint C5). The handle's `:` is sanitized into the on-disk
//! filename so the layout is portable.
//!
//! # Why this is safe
//!
//! - On Unix the secret file is **created** with mode `0600` (via
//!   `OpenOptions::mode`), so the plaintext is never momentarily group/world
//!   readable between creation and a later `chmod`.
//! - A secret is written through a `0600` temp file and atomically `rename`d into
//!   place, so a reader never observes a partially-written secret.
//! - The bytes are read back into a [`Secret`](crate::Secret), which zeroizes on
//!   drop and cannot be serialized — so resolving a secret cannot accidentally
//!   re-persist it.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CredentialVault, Secret, VaultError, VaultRef, VAULT_REF_SCHEMA_VERSION};

/// The owner-only file mode for secret material (`rw-------`).
#[cfg(unix)]
const SECRET_FILE_MODE: u32 = 0o600;
/// The owner-only directory mode for the vault root (`rwx------`).
#[cfg(unix)]
const VAULT_DIR_MODE: u32 = 0o700;

/// Extension for the file holding raw secret bytes.
const SECRET_EXT: &str = "secret";
/// Extension for the JSON metadata sidecar.
const META_EXT: &str = "meta";

/// Versioned metadata sidecar stored next to each secret.
///
/// Carries the explicit `schema_version` (constraint C5) plus the advisory
/// `name` supplied at [`put`](CredentialVault::put). It deliberately holds **no
/// secret bytes** — only the handle's descriptive metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SecretMeta {
    /// Schema version of this sidecar; see [`VAULT_REF_SCHEMA_VERSION`].
    schema_version: u16,
    /// The advisory name supplied when the secret was stored.
    name: String,
}

/// A file-backed [`CredentialVault`](crate::CredentialVault).
///
/// Each instance owns a directory under which secrets are stored one file per
/// [`VaultRef`], with `0600` permissions on Unix. The directory lives wherever
/// the caller chooses — but explicitly **not** inside the CAS object directory
/// and **not** under version control (DESIGN.md §15.4).
///
/// # Example
/// ```
/// use spork_vault::{CredentialVault, FileVault, Secret};
/// use tempfile::TempDir;
///
/// let dir = TempDir::new().unwrap();
/// let vault = FileVault::open(dir.path()).unwrap();
///
/// let r = vault.put("api_key", Secret::from("sk-12345")).unwrap();
/// // The handle is not the secret.
/// assert_ne!(r.as_str().as_bytes(), b"sk-12345");
///
/// // Round-trips back to the original bytes.
/// let got = vault.get(&r).unwrap();
/// assert_eq!(got.expose(), b"sk-12345");
///
/// // ...and can be deleted.
/// vault.delete(&r).unwrap();
/// assert!(vault.get(&r).is_err());
/// ```
#[derive(Debug, Clone)]
pub struct FileVault {
    dir: PathBuf,
}

impl FileVault {
    /// Open (creating if necessary) a `FileVault` rooted at `dir`.
    ///
    /// On Unix the directory is created/tightened to `0700`. The caller is
    /// responsible for choosing a location outside the CAS and outside git.
    ///
    /// # Errors
    /// Returns [`VaultError::Io`] if the directory cannot be created or its
    /// permissions cannot be set.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, VaultError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| VaultError::Io(e.to_string()))?;
        Self::harden_dir(&dir)?;
        Ok(FileVault { dir })
    }

    /// The directory this vault stores secrets under.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Tighten the vault directory to owner-only permissions on Unix.
    #[cfg(unix)]
    fn harden_dir(dir: &Path) -> Result<(), VaultError> {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(VAULT_DIR_MODE);
        fs::set_permissions(dir, perms).map_err(|e| VaultError::Io(e.to_string()))
    }

    /// No-op on non-Unix targets (permission model differs).
    #[cfg(not(unix))]
    fn harden_dir(_dir: &Path) -> Result<(), VaultError> {
        Ok(())
    }

    /// Translate a [`VaultRef`] into its on-disk file stem.
    ///
    /// `:` is replaced with `_` so the `"vref.1:<hex>"` handle is a portable
    /// filename. The mapping is injective over generated handles (the only `:`
    /// is the one in the prefix).
    fn stem_for(r: &VaultRef) -> String {
        r.as_str().replace(':', "_")
    }

    /// Path to the raw-secret file for `r`.
    fn secret_path(&self, r: &VaultRef) -> PathBuf {
        self.dir.join(format!("{}.{SECRET_EXT}", Self::stem_for(r)))
    }

    /// Path to the metadata sidecar for `r`.
    fn meta_path(&self, r: &VaultRef) -> PathBuf {
        self.dir.join(format!("{}.{META_EXT}", Self::stem_for(r)))
    }

    /// Create-or-truncate a file with owner-only (`0600`) permissions from the
    /// moment it exists, so secret bytes are never briefly world-readable.
    #[cfg(unix)]
    fn create_private(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(SECRET_FILE_MODE)
            .open(path)
    }

    /// Create-or-truncate a file (no Unix mode control available).
    #[cfg(not(unix))]
    fn create_private(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }

    /// Atomically write `bytes` to `final_path` through a private temp file.
    ///
    /// The temp file is created `0600`, fully written and flushed, then `rename`d
    /// over the destination — so a concurrent reader sees either the old content
    /// or the complete new content, never a torn write, and never a too-permissive
    /// secret file.
    fn atomic_write_private(final_path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
        let tmp_path = final_path.with_extension("tmp");
        {
            let mut f =
                Self::create_private(&tmp_path).map_err(|e| VaultError::Io(e.to_string()))?;
            f.write_all(bytes)
                .map_err(|e| VaultError::Io(e.to_string()))?;
            f.sync_all().map_err(|e| VaultError::Io(e.to_string()))?;
        }
        // Defensively re-assert the mode in case the destination pre-existed with
        // looser permissions and the rename inherits nothing on this platform.
        fs::rename(&tmp_path, final_path).map_err(|e| {
            // Best-effort cleanup of the temp file on failure.
            let _ = fs::remove_file(&tmp_path);
            VaultError::Io(e.to_string())
        })?;
        Self::enforce_mode(final_path)?;
        Ok(())
    }

    /// Re-assert owner-only mode on an existing file (Unix only).
    #[cfg(unix)]
    fn enforce_mode(path: &Path) -> Result<(), VaultError> {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(SECRET_FILE_MODE);
        fs::set_permissions(path, perms).map_err(|e| VaultError::Io(e.to_string()))
    }

    /// No-op on non-Unix targets.
    #[cfg(not(unix))]
    fn enforce_mode(_path: &Path) -> Result<(), VaultError> {
        Ok(())
    }
}

impl CredentialVault for FileVault {
    fn put(&self, name: &str, secret: Secret) -> Result<VaultRef, VaultError> {
        let r = VaultRef::generate();

        // Write the secret bytes first (private, atomic)...
        Self::atomic_write_private(&self.secret_path(&r), secret.expose())?;

        // ...then the metadata sidecar. If the meta write fails, roll back the
        // secret file so we don't leave an orphaned, un-described secret on disk.
        let meta = SecretMeta {
            schema_version: VAULT_REF_SCHEMA_VERSION,
            name: name.to_owned(),
        };
        let meta_bytes = serde_json::to_vec(&meta)
            .map_err(|e| VaultError::Io(format!("encoding metadata: {e}")))?;
        if let Err(e) = Self::atomic_write_private(&self.meta_path(&r), &meta_bytes) {
            let _ = fs::remove_file(self.secret_path(&r));
            return Err(e);
        }

        Ok(r)
    }

    fn get(&self, r: &VaultRef) -> Result<Secret, VaultError> {
        let path = self.secret_path(r);
        // Map a missing file to NotFound (via the From<io::Error> impl).
        let mut f = File::open(&path).map_err(VaultError::from)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| VaultError::Io(e.to_string()))?;
        // Hand the buffer straight into a Secret so the plaintext is owned by a
        // zeroizing carrier and not a bare Vec.
        Ok(Secret::new(buf))
    }

    fn delete(&self, r: &VaultRef) -> Result<(), VaultError> {
        let secret_path = self.secret_path(r);
        // The secret file is the source of truth for existence.
        match fs::remove_file(&secret_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(VaultError::NotFound),
            Err(e) => return Err(VaultError::Io(e.to_string())),
        }
        // The sidecar is best-effort: a missing meta file is not an error.
        match fs::remove_file(self.meta_path(r)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(VaultError::Io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn vault() -> (TempDir, FileVault) {
        let dir = TempDir::new().unwrap();
        let v = FileVault::open(dir.path()).unwrap();
        (dir, v)
    }

    #[test]
    fn put_get_roundtrip() {
        let (_d, v) = vault();
        let r = v.put("api_key", Secret::from("sk-roundtrip")).unwrap();
        let got = v.get(&r).unwrap();
        assert_eq!(got.expose(), b"sk-roundtrip");
    }

    #[test]
    fn vault_ref_is_not_the_secret() {
        let (_d, v) = vault();
        let secret_bytes = b"the-actual-secret-bytes";
        let r = v.put("k", Secret::new(secret_bytes.to_vec())).unwrap();
        // The opaque handle must not contain the secret bytes.
        assert!(!r
            .as_str()
            .as_bytes()
            .windows(secret_bytes.len())
            .any(|w| w == secret_bytes));
        assert_ne!(r.as_str().as_bytes(), secret_bytes);
    }

    #[test]
    fn distinct_handles_for_same_name() {
        let (_d, v) = vault();
        let r1 = v.put("dup", Secret::from("a")).unwrap();
        let r2 = v.put("dup", Secret::from("b")).unwrap();
        assert_ne!(r1, r2);
        assert_eq!(v.get(&r1).unwrap().expose(), b"a");
        assert_eq!(v.get(&r2).unwrap().expose(), b"b");
    }

    #[test]
    fn get_missing_is_not_found() {
        let (_d, v) = vault();
        let bogus = VaultRef::generate();
        assert!(matches!(v.get(&bogus), Err(VaultError::NotFound)));
    }

    #[test]
    fn delete_works_then_not_found() {
        let (_d, v) = vault();
        let r = v.put("k", Secret::from("bye")).unwrap();
        v.delete(&r).unwrap();
        assert!(matches!(v.get(&r), Err(VaultError::NotFound)));
        // Deleting again is NotFound.
        assert!(matches!(v.delete(&r), Err(VaultError::NotFound)));
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, v) = vault();
        let r = v.put("k", Secret::from("perms")).unwrap();
        let mode = fs::metadata(v.secret_path(&r))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "secret file must be owner-only");
        let meta_mode = fs::metadata(v.meta_path(&r)).unwrap().permissions().mode() & 0o777;
        assert_eq!(meta_mode, 0o600, "meta sidecar must be owner-only");
    }

    #[cfg(unix)]
    #[test]
    fn vault_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, v) = vault();
        let mode = fs::metadata(v.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "vault dir must be owner-only");
    }

    #[test]
    fn meta_sidecar_has_schema_version_and_name() {
        let (_d, v) = vault();
        let r = v.put("anthropic", Secret::from("x")).unwrap();
        let bytes = fs::read(v.meta_path(&r)).unwrap();
        let meta: SecretMeta = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(meta.schema_version, VAULT_REF_SCHEMA_VERSION);
        assert_eq!(meta.name, "anthropic");
    }

    #[test]
    fn meta_sidecar_never_contains_secret_bytes() {
        let (_d, v) = vault();
        let secret = b"PLAINTEXT-MUST-NOT-LEAK";
        let r = v.put("k", Secret::new(secret.to_vec())).unwrap();
        let meta_bytes = fs::read(v.meta_path(&r)).unwrap();
        assert!(
            !meta_bytes.windows(secret.len()).any(|w| w == secret),
            "secret bytes leaked into the metadata sidecar"
        );
    }

    #[test]
    fn reopen_same_dir_resolves_existing_secret() {
        let dir = TempDir::new().unwrap();
        let r = {
            let v = FileVault::open(dir.path()).unwrap();
            v.put("k", Secret::from("persisted")).unwrap()
        };
        // A fresh handle to the same directory still resolves the secret.
        let v2 = FileVault::open(dir.path()).unwrap();
        assert_eq!(v2.get(&r).unwrap().expose(), b"persisted");
    }

    #[test]
    fn binary_secret_roundtrip() {
        let (_d, v) = vault();
        let bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let r = v.put("blob", Secret::new(bytes.clone())).unwrap();
        assert_eq!(v.get(&r).unwrap().expose(), bytes.as_slice());
    }

    #[test]
    fn no_temp_files_left_behind() {
        let (_d, v) = vault();
        let _ = v.put("k", Secret::from("x")).unwrap();
        let leftover = fs::read_dir(v.dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().map(|x| x == "tmp").unwrap_or(false));
        assert!(!leftover, "atomic write left a .tmp file behind");
    }
}
