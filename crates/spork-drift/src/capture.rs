//! Drift capture orchestration (DESIGN.md §10.1, §10.2, §15.5).
//!
//! [`DriftCapture`] is the `captureSnapshot()` box in the §10.1 dataflow: it
//! fuses the change sources, runs the [`Attributor`](crate::Attributor), runs the
//! [`SecretScanner`](crate::SecretScanner) at capture, puts the surviving
//! *non-secret* tree into the content store ([`spork_cas`]), and records a
//! built-in `snapshot` node with `origin = auto_drift` through the F2
//! [`GraphService`](spork_graph::GraphService) (DESIGN.md §6.2, §7.1).
//!
//! # The secret invariant (DESIGN.md §15.5)
//!
//! The store is fed from a *staging tree* the capture builds itself, not the
//! live working tree: it walks the working tree (honoring the same ignore
//! profile the CAS uses), secret-scans every file's bytes, and copies only the
//! non-secret files into the staging directory (applying any unsaved editor
//! buffer in place of the on-disk bytes). The CAS then captures the staging
//! tree, so a file flagged by the scanner is *never* presented to the store and
//! no stored blob can contain it — the §15.5 "no secret enters the store"
//! invariant, by construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use spork_cas::{ObjectStore, StorageBackend};
use spork_graph::{GraphService, BUILTIN_SNAPSHOT_KIND};
use spork_hash::Hash;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use ulid::Ulid;

use crate::attribution::{AttributionRecord, Attributor, TurnContext};
use crate::error::{DriftError, Result};
use crate::secret::SecretScanner;
use crate::source::{BufferBridge, ChangeEvent, ChangeSource, ChangeSourceKind};

/// The current schema version of a [`DriftCaptureReport`] (CLAUDE.md C5).
pub const DRIFT_REPORT_SCHEMA_VERSION: u16 = 1;

/// The outcome of one [`DriftCapture::capture`] (DESIGN.md §10.2).
///
/// Carries the created drift node's id, the snapshot/root-tree hashes the store
/// produced, the [`AttributionRecord`]s for the captured changes, and — honoring
/// the §10.2 honesty requirement — the list of paths *excluded because a secret
/// was detected* (these are reported, never silently dropped). It is
/// schema-versioned (CLAUDE.md C5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftCaptureReport {
    /// Schema version of this report (CLAUDE.md C5).
    pub schema_version: u16,
    /// The id of the created `origin = auto_drift` snapshot node.
    pub node_id: Ulid,
    /// The content-addressed snapshot id the store produced.
    pub snapshot_hash: Hash,
    /// The root tree id of the captured (non-secret) working tree.
    pub root_tree: Hash,
    /// The attribution records for the captured changes (A.3).
    pub attribution: Vec<AttributionRecord>,
    /// Repo-relative paths excluded because the secret scanner flagged them
    /// (reported, not silently dropped — DESIGN.md §10.2, §15.5).
    pub excluded_secrets: Vec<String>,
}

/// The drift-capture orchestrator (DESIGN.md §10.1 `captureSnapshot()`).
///
/// Owns the working-tree root, the ignore profile/matcher (the same exclusion
/// machinery the CAS uses), and the [`SecretScanner`]. It does **not** own the
/// store or graph — those are passed to [`capture`](DriftCapture::capture) so a
/// daemon can keep a single `ObjectStore`/`GraphService` and drive many captures
/// through it.
pub struct DriftCapture {
    /// The working-tree root being watched for drift.
    root: PathBuf,
    /// The ignore profile (its hash is the snapshot's `ignore_profile_hash`).
    profile: IgnoreProfile,
    /// The compiled ignore matcher derived from `profile`.
    matcher: IgnoreMatcher,
    /// The secret scanner run at capture (DESIGN.md §15.5).
    scanner: SecretScanner,
    /// The A.3 debounce window for attribution.
    debounce: Duration,
}

impl DriftCapture {
    /// Create a capture over `root` using the default ignore profile and the
    /// default secret-pattern set, with a 250 ms debounce window.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self::with_profile(root, IgnoreProfile::default_profile())
    }

    /// Create a capture over `root` with an explicit ignore profile.
    #[must_use]
    pub fn with_profile(root: impl AsRef<Path>, profile: IgnoreProfile) -> Self {
        let matcher = IgnoreMatcher::new(&profile);
        DriftCapture {
            root: root.as_ref().to_path_buf(),
            profile,
            matcher,
            scanner: SecretScanner::with_default_patterns(),
            debounce: Duration::from_millis(250),
        }
    }

    /// Override the secret scanner (e.g. to add an additive custom pattern set).
    #[must_use]
    pub fn with_scanner(mut self, scanner: SecretScanner) -> Self {
        self.scanner = scanner;
        self
    }

    /// Override the A.3 debounce window.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// The working-tree root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Borrow the secret scanner.
    #[must_use]
    pub fn scanner(&self) -> &SecretScanner {
        &self.scanner
    }

    /// Drain a batch of change sources and attribute every change under `ctx`,
    /// applying the A.3 precedence (interceptor > watcher) and the
    /// rescan-only-adds rule.
    ///
    /// Returns the produced records. This is the *fuse → attribute* half of the
    /// pipeline; [`capture`](DriftCapture::capture) is the *secret-scan → store →
    /// node* half. They are separate so a caller can attribute on a fast turn
    /// boundary but capture less often (DESIGN.md §10.2 debounce-on-quiescence).
    pub fn fuse_and_attribute(
        &self,
        sources: &mut [&mut dyn ChangeSource],
        ctx: &TurnContext,
    ) -> Vec<AttributionRecord> {
        let mut attributor = Attributor::new(self.debounce);
        // Non-rescan sources first (interceptor/buffer/watcher) so their evidence
        // is in place before the rescan's "only add" pass runs.
        let mut rescan_events: Vec<ChangeEvent> = Vec::new();
        let mut primary_events: Vec<ChangeEvent> = Vec::new();
        for src in sources.iter_mut() {
            let is_rescan = src.kind() == ChangeSourceKind::Rescan;
            let events = src.poll();
            if is_rescan {
                rescan_events.extend(events);
            } else {
                primary_events.extend(events);
            }
        }
        attributor.attribute(&primary_events, ctx);
        attributor.ingest_rescan(&rescan_events, ctx);
        attributor.records()
    }

    /// Capture the current working tree as an `origin = auto_drift` snapshot
    /// node, excluding secret files (DESIGN.md §10.1, §10.2, §15.5).
    ///
    /// Steps, in order:
    /// 1. Walk the working tree honoring the ignore profile, secret-scanning each
    ///    file; build a *staging tree* of only non-secret files, applying any
    ///    unsaved editor buffer in place of the on-disk bytes.
    /// 2. Put the staging tree into the store and create a snapshot object over
    ///    it (so no secret blob is ever presented to the CAS).
    /// 3. Create a built-in `snapshot` node with `origin = auto_drift` (and the
    ///    A.3 attribution + excluded-secret list in its payload) through the
    ///    graph service, parented at `parents`.
    ///
    /// `attribution` is the records from [`fuse_and_attribute`](DriftCapture::fuse_and_attribute);
    /// `buffers` (optional) supplies unsaved editor content to materialize.
    ///
    /// # Errors
    /// - [`DriftError::Io`] on a walk/copy failure.
    /// - [`DriftError::Cas`] on a content-store failure.
    /// - [`DriftError::Graph`] on a graph-append failure.
    pub fn capture<S: StorageBackend + Sync>(
        &self,
        store: &ObjectStore<S>,
        graph: &mut GraphService,
        parents: Vec<Ulid>,
        branch_id: &str,
        attribution: Vec<AttributionRecord>,
        buffers: Option<&BufferBridge>,
    ) -> Result<DriftCaptureReport> {
        // (1) Build the non-secret staging tree.
        let staging = tempdir_in_root_parent(&self.root)?;
        let mut excluded: Vec<String> = Vec::new();
        self.stage_tree(
            &self.root,
            Path::new(""),
            staging.path(),
            buffers,
            &mut excluded,
        )?;
        excluded.sort();
        excluded.dedup();

        // (2) Capture the staging tree into the store. Because only non-secret
        // files were copied into `staging`, no secret can reach a CAS blob.
        let ignore_profile_hash = self.profile.hash();
        let (snapshot_hash, root_tree, _stats) = store
            .capture_snapshot(staging.path(), &self.matcher, ignore_profile_hash, None)
            .map_err(|e| DriftError::Cas(e.to_string()))?;

        // (3) Record the auto-drift node through the graph service.
        let payload = serde_json::json!({
            "origin": "auto_drift",
            "drift_source": "fused",
            "attribution": attribution,
            "excluded_secrets": excluded,
            "report_schema_version": DRIFT_REPORT_SCHEMA_VERSION,
        });
        let envelope = graph
            .create_node(
                BUILTIN_SNAPSHOT_KIND,
                None,
                parents,
                branch_id,
                payload,
                true,
                Some(snapshot_hash),
            )
            .map_err(|e| DriftError::Graph(e.to_string()))?;

        Ok(DriftCaptureReport {
            schema_version: DRIFT_REPORT_SCHEMA_VERSION,
            node_id: envelope.id,
            snapshot_hash,
            root_tree,
            attribution,
            excluded_secrets: excluded,
        })
    }

    /// Recursively copy the kept, non-secret entries of `abs` (relative path
    /// `rel`) into `dest`, scanning each file and recording excluded paths.
    fn stage_tree(
        &self,
        abs: &Path,
        rel: &Path,
        dest: &Path,
        buffers: Option<&BufferBridge>,
        excluded: &mut Vec<String>,
    ) -> Result<()> {
        let rd = std::fs::read_dir(abs).map_err(|e| DriftError::io(abs, &e))?;
        for entry in rd {
            let entry = entry.map_err(|e| DriftError::io(abs, &e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let child_abs = entry.path();
            let child_rel = rel.join(name);
            let meta = std::fs::symlink_metadata(&child_abs)
                .map_err(|e| DriftError::io(&child_abs, &e))?;
            let is_dir = meta.is_dir();
            if self.matcher.is_ignored(&child_rel, is_dir) {
                continue;
            }
            // Skip the staging directory itself if it happens to live under root.
            if child_abs == dest {
                continue;
            }
            let child_dest = dest.join(name);

            if meta.file_type().is_symlink() {
                // Symlink text never holds a chunked file blob; copy it through.
                let target =
                    std::fs::read_link(&child_abs).map_err(|e| DriftError::io(&child_abs, &e))?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &child_dest)
                    .map_err(|e| DriftError::io(&child_dest, &e))?;
                #[cfg(not(unix))]
                std::fs::write(&child_dest, target.to_string_lossy().as_bytes())
                    .map_err(|e| DriftError::io(&child_dest, &e))?;
            } else if is_dir {
                std::fs::create_dir_all(&child_dest)
                    .map_err(|e| DriftError::io(&child_dest, &e))?;
                self.stage_tree(&child_abs, &child_rel, &child_dest, buffers, excluded)?;
                // Drop an empty staged dir so an all-secret directory does not
                // appear as an empty dir in the snapshot.
                if dir_is_empty(&child_dest)? {
                    let _ = std::fs::remove_dir(&child_dest);
                }
            } else if meta.is_file() {
                // Prefer the unsaved editor buffer if one is dirty for this path,
                // so the node reflects what the human is looking at (§10.2).
                let bytes = match buffers.and_then(|b| b.buffer_bytes(&child_rel)) {
                    Some(buf) => buf.to_vec(),
                    None => {
                        std::fs::read(&child_abs).map_err(|e| DriftError::io(&child_abs, &e))?
                    }
                };
                // Secret-scan at capture: a match excludes the file entirely so
                // it never reaches the store (DESIGN.md §15.5).
                if self.scanner.scan(&bytes) {
                    excluded.push(canonical(&child_rel));
                    continue;
                }
                std::fs::write(&child_dest, &bytes).map_err(|e| DriftError::io(&child_dest, &e))?;
                copy_mode(&meta, &child_dest)?;
            }
        }
        Ok(())
    }

    /// Compute the per-path content-hash baseline of the current (non-secret)
    /// staging view, for seeding a [`ReconciliationRescan`](crate::ReconciliationRescan)
    /// after a capture so the next rescan diffs against what was captured.
    ///
    /// # Errors
    /// Returns [`DriftError::Io`] on a walk/read failure.
    pub fn content_baseline(
        &self,
        buffers: Option<&BufferBridge>,
    ) -> Result<BTreeMap<PathBuf, Hash>> {
        let mut out = BTreeMap::new();
        self.baseline_walk(&self.root, Path::new(""), buffers, &mut out)?;
        Ok(out)
    }

    fn baseline_walk(
        &self,
        abs: &Path,
        rel: &Path,
        buffers: Option<&BufferBridge>,
        out: &mut BTreeMap<PathBuf, Hash>,
    ) -> Result<()> {
        let rd = std::fs::read_dir(abs).map_err(|e| DriftError::io(abs, &e))?;
        for entry in rd {
            let entry = entry.map_err(|e| DriftError::io(abs, &e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let child_abs = entry.path();
            let child_rel = rel.join(name);
            let meta = std::fs::symlink_metadata(&child_abs)
                .map_err(|e| DriftError::io(&child_abs, &e))?;
            let is_dir = meta.is_dir();
            if self.matcher.is_ignored(&child_rel, is_dir) {
                continue;
            }
            if meta.file_type().is_symlink() {
                let target =
                    std::fs::read_link(&child_abs).map_err(|e| DriftError::io(&child_abs, &e))?;
                out.insert(
                    child_rel,
                    spork_hash::hash_bytes(target.as_os_str().to_string_lossy().as_bytes()),
                );
            } else if is_dir {
                self.baseline_walk(&child_abs, &child_rel, buffers, out)?;
            } else if meta.is_file() {
                let bytes = match buffers.and_then(|b| b.buffer_bytes(&child_rel)) {
                    Some(buf) => buf.to_vec(),
                    None => {
                        std::fs::read(&child_abs).map_err(|e| DriftError::io(&child_abs, &e))?
                    }
                };
                out.insert(child_rel, spork_hash::hash_bytes(&bytes));
            }
        }
        Ok(())
    }
}

/// Render a repo-relative path as the canonical `/`-separated string.
fn canonical(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `dir` has no entries.
fn dir_is_empty(dir: &Path) -> Result<bool> {
    let mut rd = std::fs::read_dir(dir).map_err(|e| DriftError::io(dir, &e))?;
    Ok(rd.next().is_none())
}

/// Copy the source file's permission mode onto the staged copy (Unix only).
#[cfg(unix)]
fn copy_mode(meta: &std::fs::Metadata, dest: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let perms = std::fs::Permissions::from_mode(meta.permissions().mode());
    std::fs::set_permissions(dest, perms).map_err(|e| DriftError::io(dest, &e))
}

/// No-op on non-Unix (modes are not POSIX bits there).
#[cfg(not(unix))]
fn copy_mode(_meta: &std::fs::Metadata, _dest: &Path) -> Result<()> {
    Ok(())
}

/// A self-cleaning staging directory created next to the working-tree root.
///
/// Built next to (not inside) `root` so it is never itself walked as part of the
/// tree, and removed on drop so a capture leaves no residue.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Create a unique staging directory beside `root` (in its parent, or the system
/// temp dir if `root` has no parent), so the staging tree is never inside the
/// tree being captured.
fn tempdir_in_root_parent(root: &Path) -> Result<Staging> {
    let base = root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let unique = format!(".spork-drift-stage-{}", Ulid::new());
    let path = base.join(unique);
    std::fs::create_dir_all(&path).map_err(|e| DriftError::io(&path, &e))?;
    Ok(Staging { path })
}
