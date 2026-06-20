//! Opening the on-disk object store the subcommands operate on.
//!
//! Every subcommand addresses a single physical store identified by the
//! `--store` path (default `.spork/objects`). This module centralizes how that
//! path is turned into a live [`ObjectStore`] over the v1 [`LooseStore`]
//! backend, so the commands stay focused on their own logic and all share one
//! definition of "where the objects live".
//!
//! # Path semantics
//!
//! [`LooseStore::open(root)`](LooseStore::open) always places its loose tree at
//! `root/objects/<aa>/...`. To make the user-facing `--store` value read
//! naturally — and to make the default `.spork/objects` land objects directly at
//! `.spork/objects/<aa>/...` rather than the doubled `.spork/objects/objects/` —
//! [`open_store`] resolves the backend root as follows:
//!
//! - If the final component of `--store` is literally `objects` (the default and
//!   the conventional layout), open its **parent**, so the backend recreates that
//!   exact `objects` directory.
//! - Otherwise, treat `--store` itself as the root, so objects live at
//!   `<store>/objects/<aa>/...`.
//!
//! Either branch is a pure function of the path, so a given `--store` value
//! always resolves to the same physical store across invocations — which is what
//! lets `chunk-stats` and a warm `put-tree` find the cold pass's objects.
//!
//! Design references: DESIGN.md §10.1 (loose objects + packfiles).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use spork_cas::{LooseStore, ObjectStore};

/// The default store location, relative to the current working directory.
///
/// Resolves (see [`open_store`]) to a loose tree at `.spork/objects/<aa>/...`.
pub const DEFAULT_STORE: &str = ".spork/objects";

/// Open (creating as needed) the object store identified by `store_path`.
///
/// See the [module docs](crate::store) for the path-resolution rules. The
/// returned store's loose `objects/` directory is a deterministic function of
/// `store_path`, so repeated invocations with the same value address the same
/// physical objects.
///
/// # Errors
/// Returns an error if the store directory cannot be created.
pub fn open_store(store_path: &Path) -> Result<ObjectStore<LooseStore>> {
    let root = backend_root(store_path);
    let backend = LooseStore::open(&root)
        .with_context(|| format!("opening object store at {}", store_path.display()))?;
    Ok(ObjectStore::new(backend))
}

/// Resolve the root to hand [`LooseStore::open`] for a given `--store` value.
///
/// When `store_path` ends in an `objects` component, the backend root is its
/// parent (so the recreated `objects` dir is exactly `store_path`); otherwise the
/// root is `store_path` itself.
fn backend_root(store_path: &Path) -> PathBuf {
    if store_path.file_name() == Some(OsStr::new("objects")) {
        match store_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            // A bare `objects` with no parent → current directory.
            _ => PathBuf::from("."),
        }
    } else {
        store_path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_cas::StorageBackend;
    use tempfile::TempDir;

    #[test]
    fn objects_suffixed_path_maps_to_itself() {
        let tmp = TempDir::new().unwrap();
        let store_path = tmp.path().join(".spork").join("objects");
        let store = open_store(&store_path).unwrap();
        // The loose `objects/` directory is exactly the requested path.
        assert_eq!(store.backend().objects_dir(), store_path.as_path());

        let (id, _) = store.put_blob_bytes(b"hi").unwrap();
        assert_eq!(store.read_blob(&id).unwrap(), b"hi");
        let hex = id.to_hex();
        assert!(store_path.join(&hex[..2]).join(&hex[2..]).exists());
    }

    #[test]
    fn non_objects_path_gets_an_objects_subdir() {
        let tmp = TempDir::new().unwrap();
        let store_path = tmp.path().join("my-store");
        let store = open_store(&store_path).unwrap();
        // Objects live under `<store>/objects`.
        assert_eq!(
            store.backend().objects_dir(),
            store_path.join("objects").as_path()
        );
        let (id, _) = store.put_blob_bytes(b"hello").unwrap();
        let hex = id.to_hex();
        assert!(store_path
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
            .exists());
    }

    #[test]
    fn resolution_is_stable_across_calls() {
        // The same `--store` value must resolve to the same physical store so a
        // warm re-capture sees the cold pass's objects.
        let tmp = TempDir::new().unwrap();
        let store_path = tmp.path().join("repo");
        let a = open_store(&store_path).unwrap();
        let (id, _) = a.put_blob_bytes(b"shared").unwrap();
        let b = open_store(&store_path).unwrap();
        assert!(b.backend().has(&id).unwrap());
    }
}
