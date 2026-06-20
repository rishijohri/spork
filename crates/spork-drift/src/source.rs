//! Change sources and the fused [`ChangeEvent`] stream (DESIGN.md §10.1, §10.2).
//!
//! Spork closes the untracked-mutation gap by fusing **three change-capture
//! sources** (plus an editor-buffer bridge) instead of trusting edit-tool deltas
//! alone (DESIGN.md §10.2):
//!
//! - [`EditInterceptor`] — the in-process interceptor: precise, low-latency
//!   *who/which-turn* attribution for agent and IDE writes.
//! - [`FsWatcher`] — the OS filesystem watcher (`notify`: FSEvents / inotify /
//!   ReadDirectoryChangesW): authoritative on *whether* something changed,
//!   catching bash `rm`/`mv`, external editors, build scripts.
//! - [`ReconciliationRescan`] — the mandatory backstop: a content-hash walk of
//!   the tree against the last snapshot, because watchers are lossy under load
//!   and on network/virtual filesystems (DESIGN.md §10.2).
//! - [`BufferBridge`] — the editor-buffer bridge: flushes UNSAVED buffers so a
//!   node reflects what the human is looking at, not just what is on disk.
//!
//! Each source implements the [`ChangeSource`] seam and `poll`s a batch of
//! [`ChangeEvent`]s. The [`Attributor`](crate::Attributor) fuses them into one
//! authoritative per-node snapshot (DESIGN.md §10.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{DriftError, Result};

/// Which of the fused capture sources produced a [`ChangeEvent`].
///
/// The kind is the primary evidence the [`Attributor`](crate::Attributor) weighs
/// under the A.3 precedence rule: in-process [`Interceptor`](ChangeSourceKind::Interceptor)
/// evidence outranks [`FsWatcher`](ChangeSourceKind::FsWatcher) evidence for the
/// same path within the same turn (DESIGN.md A.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ChangeSourceKind {
    /// The in-process edit interceptor (agent + IDE writes). Authoritative on
    /// *who*.
    Interceptor,
    /// The OS filesystem watcher. Authoritative on *whether* something changed.
    FsWatcher,
    /// The periodic content-hash reconciliation rescan (the backstop).
    Rescan,
    /// The editor-buffer bridge (unsaved buffers).
    BufferBridge,
}

impl ChangeSourceKind {
    /// A short human label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ChangeSourceKind::Interceptor => "interceptor",
            ChangeSourceKind::FsWatcher => "fs-watcher",
            ChangeSourceKind::Rescan => "rescan",
            ChangeSourceKind::BufferBridge => "buffer-bridge",
        }
    }
}

/// The kind of mutation a [`ChangeEvent`] reports for a path.
///
/// These are the out-of-band mutations §10.2 must capture: a bash `rm`
/// ([`Removed`](ChangeOp::Removed)), an `mv`/codegen `add`
/// ([`Added`](ChangeOp::Added)), and an in-place edit
/// ([`Modified`](ChangeOp::Modified)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ChangeOp {
    /// The path appeared (a new file, an `mv` destination, codegen output).
    Added,
    /// The path's contents changed in place.
    Modified,
    /// The path disappeared (a bash `rm`, an `mv` source).
    Removed,
}

impl ChangeOp {
    /// A short human label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ChangeOp::Added => "added",
            ChangeOp::Modified => "modified",
            ChangeOp::Removed => "removed",
        }
    }
}

/// One observed change to one repo-relative path, tagged with its source.
///
/// Events carry an `observed_at` [`Instant`] so the [`Attributor`](crate::Attributor)
/// can apply the A.3 debounce-window heuristic (was this within the debounce
/// window of a known tool/bash exec?) without a wall-clock dependency in
/// identity (DESIGN.md A.3, A.5). `turn` carries the active agent turn id when
/// the producing source knew it (the interceptor always does; the watcher and
/// rescan never do).
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    /// The repo-relative path that changed (always relative to the working-tree
    /// root, using `/` separators as the canonical form).
    pub path: PathBuf,
    /// What kind of mutation this is.
    pub op: ChangeOp,
    /// Which source observed it.
    pub source: ChangeSourceKind,
    /// The active agent turn id at observation time, if the source knew it.
    ///
    /// The interceptor sets this (it owns the turn boundary); the fs-watcher and
    /// rescan leave it `None` (they cannot see turns) and the attributor fills
    /// the gap heuristically (DESIGN.md A.3 Q3/Q4).
    pub turn: Option<u64>,
    /// When the change was observed (monotonic; for the debounce heuristic).
    pub observed_at: Instant,
}

impl ChangeEvent {
    /// Construct a change event observed *now*.
    #[must_use]
    pub fn now(
        path: impl Into<PathBuf>,
        op: ChangeOp,
        source: ChangeSourceKind,
        turn: Option<u64>,
    ) -> Self {
        ChangeEvent {
            path: path.into(),
            op,
            source,
            turn,
            observed_at: Instant::now(),
        }
    }
}

/// The capture-source seam (CLAUDE.md C3: a trait is a seam, one real impl class
/// per source now).
///
/// A source is polled for a batch of pending [`ChangeEvent`]s; `poll` drains the
/// source's queue (the next `poll` returns only newly observed events). The
/// pipeline polls all sources and hands the union to the
/// [`Attributor`](crate::Attributor). The trait deliberately exposes only `kind`
/// and `poll` so the fusion layer treats every source uniformly (DESIGN.md
/// §10.1 "ChangeEvent Fusion").
pub trait ChangeSource {
    /// Which source this is (drives A.3 precedence).
    fn kind(&self) -> ChangeSourceKind;

    /// Drain and return the events observed since the last `poll`.
    fn poll(&mut self) -> Vec<ChangeEvent>;
}

/// The in-process edit interceptor: precise *who/which-turn* attribution.
///
/// In the headless core this is driven directly: the agent/IDE write path calls
/// [`record`](EditInterceptor::record) as it mutates a file, stamping the active
/// turn. Because the interceptor sees the write at the moment it happens, its
/// evidence is authoritative on *who* and outranks the fs-watcher for the same
/// `(path, turn)` (DESIGN.md A.3 precedence).
#[derive(Debug, Default)]
pub struct EditInterceptor {
    /// The active turn the interceptor stamps on recorded edits.
    turn: Option<u64>,
    /// Pending events drained by `poll`.
    pending: Vec<ChangeEvent>,
}

impl EditInterceptor {
    /// Create an interceptor with no active turn.
    #[must_use]
    pub fn new() -> Self {
        EditInterceptor::default()
    }

    /// Open an agent turn; subsequent [`record`](EditInterceptor::record) calls
    /// are stamped with `turn`.
    pub fn begin_turn(&mut self, turn: u64) {
        self.turn = Some(turn);
    }

    /// Close the active turn; later edits are no longer agent-attributed by the
    /// interceptor (the attributor falls through to its heuristics).
    pub fn end_turn(&mut self) {
        self.turn = None;
    }

    /// Record that the agent/IDE write path mutated `path` with `op`.
    ///
    /// The event is stamped with the active turn (if any) and the
    /// [`Interceptor`](ChangeSourceKind::Interceptor) source kind.
    pub fn record(&mut self, path: impl Into<PathBuf>, op: ChangeOp) {
        self.pending.push(ChangeEvent::now(
            path,
            op,
            ChangeSourceKind::Interceptor,
            self.turn,
        ));
    }
}

impl ChangeSource for EditInterceptor {
    fn kind(&self) -> ChangeSourceKind {
        ChangeSourceKind::Interceptor
    }

    fn poll(&mut self) -> Vec<ChangeEvent> {
        std::mem::take(&mut self.pending)
    }
}

/// The OS filesystem watcher (via `notify`): authoritative on *whether*
/// something changed.
///
/// This catches everything the interceptor cannot see — bash commands, external
/// editors, build scripts, out-of-band git operations (DESIGN.md §10.2). The
/// watcher runs `notify`'s recommended backend (FSEvents on macOS, inotify on
/// Linux, ReadDirectoryChangesW on Windows) and translates raw events into
/// repo-relative [`ChangeEvent`]s, coalescing within a debounce window so a
/// rename storm does not flood the fusion layer.
///
/// The watcher *never* knows the agent turn (`turn` is always `None`); the
/// attributor supplies attribution heuristically (DESIGN.md A.3).
pub struct FsWatcher {
    /// The watched working-tree root; events are reported relative to it.
    root: PathBuf,
    /// The live `notify` watcher (kept alive for the watch to stay armed).
    _watcher: notify::RecommendedWatcher,
    /// The channel raw `notify` events arrive on.
    rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    /// Coalescing window: events for the same `(path, op)` within this window
    /// are merged so a rename storm does not flood fusion (DESIGN.md §10.4
    /// "snapshot-on-quiescence debouncing").
    debounce: Duration,
}

impl FsWatcher {
    /// Arm a recursive watch on `root`.
    ///
    /// # Errors
    /// Returns [`DriftError::Io`] if the watcher cannot be created or the watch
    /// cannot be armed (e.g. the path does not exist).
    pub fn watch(root: impl AsRef<Path>, debounce: Duration) -> Result<Self> {
        use notify::Watcher as _;
        let root = root.as_ref().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            // A closed receiver (the FsWatcher was dropped) is not an error
            // worth panicking the notify thread over; just drop the event.
            let _ = tx.send(res);
        })
        .map_err(|e| DriftError::Io(format!("notify init: {e}")))?;
        watcher
            .watch(&root, notify::RecursiveMode::Recursive)
            .map_err(|e| DriftError::Io(format!("notify watch {}: {e}", root.display())))?;
        Ok(FsWatcher {
            root,
            _watcher: watcher,
            rx,
            debounce,
        })
    }

    /// Translate a raw `notify` event kind into our [`ChangeOp`], if it is a
    /// content/existence change we capture.
    fn op_for(kind: &notify::EventKind) -> Option<ChangeOp> {
        use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind, RenameMode};
        match kind {
            EventKind::Create(CreateKind::File | CreateKind::Any) => Some(ChangeOp::Added),
            EventKind::Remove(RemoveKind::File | RemoveKind::Any) => Some(ChangeOp::Removed),
            // A rename "From" is the source vanishing; a rename "To"/"Both" is a
            // new path appearing. Treat plain content/metadata modifies as edits.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some(ChangeOp::Removed),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some(ChangeOp::Added),
            EventKind::Modify(_) => Some(ChangeOp::Modified),
            _ => None,
        }
    }
}

impl ChangeSource for FsWatcher {
    fn kind(&self) -> ChangeSourceKind {
        ChangeSourceKind::FsWatcher
    }

    fn poll(&mut self) -> Vec<ChangeEvent> {
        // Drain everything currently queued, coalescing by (path, op) within the
        // debounce window (last-writer-wins on the timestamp).
        let mut coalesced: BTreeMap<(PathBuf, ChangeOp), Instant> = BTreeMap::new();
        while let Ok(res) = self.rx.try_recv() {
            let Ok(event) = res else { continue };
            let Some(op) = Self::op_for(&event.kind) else {
                continue;
            };
            for raw in event.paths {
                // Report repo-relative paths; ignore anything outside the root.
                let rel = match raw.strip_prefix(&self.root) {
                    Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
                    _ => continue,
                };
                let now = Instant::now();
                coalesced
                    .entry((rel, op))
                    .and_modify(|t| {
                        // Within the debounce window, keep the latest timestamp;
                        // beyond it, this is genuinely a fresh observation.
                        if now.duration_since(*t) <= self.debounce {
                            *t = now;
                        }
                    })
                    .or_insert(now);
            }
        }
        coalesced
            .into_iter()
            .map(|((path, op), observed_at)| ChangeEvent {
                path,
                op,
                source: ChangeSourceKind::FsWatcher,
                turn: None,
                observed_at,
            })
            .collect()
    }
}

/// The reconciliation rescan: a content-hash walk of the tree vs. the last
/// snapshot (the mandatory backstop).
///
/// Watchers are notoriously lossy under load, on network/virtual filesystems,
/// and during rename storms (DESIGN.md §10.2). The rescan walks the working tree
/// honoring the same ignore profile the CAS uses, hashes each kept file's bytes,
/// and diffs the result against a caller-supplied *baseline* (the last captured
/// snapshot's per-path content hashes). It emits an [`Added`](ChangeOp::Added) /
/// [`Modified`](ChangeOp::Modified) / [`Removed`](ChangeOp::Removed) event for
/// every divergence.
///
/// Per A.3 the rescan only ever *adds* drift records; it never knows a turn
/// (`turn` is always `None`) and the attributor never lets it rewrite an
/// existing high-confidence record (DESIGN.md A.3).
pub struct ReconciliationRescan {
    /// The working-tree root to walk.
    root: PathBuf,
    /// The ignore matcher (same exclusion machinery the CAS uses).
    matcher: spork_ignore::IgnoreMatcher,
    /// The last-snapshot baseline: repo-relative path -> content hash.
    baseline: BTreeMap<PathBuf, spork_hash::Hash>,
    /// Events produced by the most recent [`rescan`](ReconciliationRescan::rescan),
    /// drained by `poll`.
    pending: Vec<ChangeEvent>,
}

impl ReconciliationRescan {
    /// Create a rescan over `root` with `matcher` and an empty baseline.
    ///
    /// An empty baseline means a first rescan reports every kept file as
    /// [`Added`](ChangeOp::Added) — the correct behavior for the very first
    /// capture of a tree.
    #[must_use]
    pub fn new(root: impl AsRef<Path>, matcher: spork_ignore::IgnoreMatcher) -> Self {
        ReconciliationRescan {
            root: root.as_ref().to_path_buf(),
            matcher,
            baseline: BTreeMap::new(),
            pending: Vec::new(),
        }
    }

    /// Replace the baseline with the content hashes from the last capture.
    ///
    /// Call this after a capture so the *next* rescan diffs against the state
    /// just captured (so already-captured changes are not re-reported).
    pub fn set_baseline(&mut self, baseline: BTreeMap<PathBuf, spork_hash::Hash>) {
        self.baseline = baseline;
    }

    /// Walk the tree, hash every kept file, and return the per-path content map.
    ///
    /// This is the same gitignore-aware exclusion walk the CAS uses, so the
    /// rescan's view matches what would actually be stored (DESIGN.md §10.4,
    /// §10.5). Symlinks are hashed by their target-path bytes (matching how the
    /// CAS content-addresses a link's text).
    ///
    /// # Errors
    /// Returns [`DriftError::Io`] on a directory-read or file-read failure.
    pub fn scan_tree(&self) -> Result<BTreeMap<PathBuf, spork_hash::Hash>> {
        let mut out = BTreeMap::new();
        self.walk(&self.root, Path::new(""), &mut out)?;
        Ok(out)
    }

    fn walk(
        &self,
        abs: &Path,
        rel: &Path,
        out: &mut BTreeMap<PathBuf, spork_hash::Hash>,
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
                self.walk(&child_abs, &child_rel, out)?;
            } else if meta.is_file() {
                let bytes =
                    std::fs::read(&child_abs).map_err(|e| DriftError::io(&child_abs, &e))?;
                out.insert(child_rel, spork_hash::hash_bytes(&bytes));
            }
        }
        Ok(())
    }

    /// Run a rescan: diff the current tree against the baseline and queue one
    /// [`ChangeEvent`] per divergence.
    ///
    /// # Errors
    /// Returns [`DriftError::Io`] on a walk failure.
    pub fn rescan(&mut self) -> Result<()> {
        let current = self.scan_tree()?;
        let mut events = Vec::new();
        for (path, hash) in &current {
            match self.baseline.get(path) {
                Some(prev) if prev == hash => {}
                Some(_) => {
                    events.push(ChangeEvent::now(
                        path.clone(),
                        ChangeOp::Modified,
                        ChangeSourceKind::Rescan,
                        None,
                    ));
                }
                None => {
                    events.push(ChangeEvent::now(
                        path.clone(),
                        ChangeOp::Added,
                        ChangeSourceKind::Rescan,
                        None,
                    ));
                }
            }
        }
        for path in self.baseline.keys() {
            if !current.contains_key(path) {
                events.push(ChangeEvent::now(
                    path.clone(),
                    ChangeOp::Removed,
                    ChangeSourceKind::Rescan,
                    None,
                ));
            }
        }
        self.pending.extend(events);
        Ok(())
    }
}

impl ChangeSource for ReconciliationRescan {
    fn kind(&self) -> ChangeSourceKind {
        ChangeSourceKind::Rescan
    }

    fn poll(&mut self) -> Vec<ChangeEvent> {
        std::mem::take(&mut self.pending)
    }
}

/// The editor-buffer bridge: flushes UNSAVED buffers (DESIGN.md §10.2).
///
/// A node should reflect what the human is actually looking at, not just what is
/// on disk, so the bridge surfaces unsaved/dirty editor buffers as change
/// events. In a real desktop build this is fed by an LSP/IDE plugin; in the
/// headless core it is driven directly via [`set_buffer`](BufferBridge::set_buffer),
/// which marks a path dirty (and, when `focused`, lets the attributor classify
/// it as a high-confidence human edit per A.3 Q2).
///
/// The bridge also lets the pipeline materialize the unsaved content at capture
/// time so the drift node reflects the buffer, not the stale on-disk bytes; the
/// buffer bytes are retrievable via [`buffer_bytes`](BufferBridge::buffer_bytes).
#[derive(Debug, Default)]
pub struct BufferBridge {
    /// Dirty buffers: repo-relative path -> (unsaved bytes, focused?).
    buffers: BTreeMap<PathBuf, (Vec<u8>, bool)>,
    /// Pending dirty-buffer events drained by `poll`.
    pending: Vec<ChangeEvent>,
}

impl BufferBridge {
    /// Create an empty bridge with no dirty buffers.
    #[must_use]
    pub fn new() -> Self {
        BufferBridge::default()
    }

    /// Mark `path` as having unsaved buffer `contents`; `focused` records
    /// whether that buffer is the one the human is currently editing (A.3 Q2).
    ///
    /// Emits a [`Modified`](ChangeOp::Modified) [`BufferBridge`](ChangeSourceKind::BufferBridge)
    /// event so the fusion layer treats the unsaved edit like any other change.
    pub fn set_buffer(&mut self, path: impl Into<PathBuf>, contents: Vec<u8>, focused: bool) {
        let path = path.into();
        self.buffers.insert(path.clone(), (contents, focused));
        self.pending.push(ChangeEvent::now(
            path,
            ChangeOp::Modified,
            ChangeSourceKind::BufferBridge,
            None,
        ));
    }

    /// Whether `path` currently has a dirty, *focused* buffer (A.3 Q2 input).
    #[must_use]
    pub fn is_focused_dirty(&self, path: &Path) -> bool {
        self.buffers
            .get(path)
            .map(|(_, focused)| *focused)
            .unwrap_or(false)
    }

    /// The unsaved bytes for `path`, if a buffer is dirty.
    ///
    /// The capture stage uses this to materialize the buffer content into the
    /// snapshot in place of the stale on-disk bytes (DESIGN.md §10.2).
    #[must_use]
    pub fn buffer_bytes(&self, path: &Path) -> Option<&[u8]> {
        self.buffers.get(path).map(|(bytes, _)| bytes.as_slice())
    }
}

impl ChangeSource for BufferBridge {
    fn kind(&self) -> ChangeSourceKind {
        ChangeSourceKind::BufferBridge
    }

    fn poll(&mut self) -> Vec<ChangeEvent> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_ignore::{IgnoreMatcher, IgnoreProfile};
    use std::fs;

    fn matcher() -> IgnoreMatcher {
        IgnoreMatcher::new(&IgnoreProfile::default_profile())
    }

    #[test]
    fn interceptor_stamps_active_turn_and_drains() {
        let mut int = EditInterceptor::new();
        assert_eq!(int.kind(), ChangeSourceKind::Interceptor);
        int.begin_turn(42);
        int.record("src/a.rs", ChangeOp::Modified);
        int.record("src/b.rs", ChangeOp::Added);
        let events = int.poll();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.turn == Some(42)));
        assert!(events
            .iter()
            .all(|e| e.source == ChangeSourceKind::Interceptor));
        // A second poll is empty (the queue was drained).
        assert!(int.poll().is_empty());

        int.end_turn();
        int.record("src/c.rs", ChangeOp::Removed);
        assert_eq!(int.poll()[0].turn, None);
    }

    #[test]
    fn buffer_bridge_tracks_focus_and_bytes() {
        let mut bb = BufferBridge::new();
        assert_eq!(bb.kind(), ChangeSourceKind::BufferBridge);
        bb.set_buffer("src/x.rs", b"unsaved".to_vec(), true);
        bb.set_buffer("src/y.rs", b"draft".to_vec(), false);

        assert!(bb.is_focused_dirty(Path::new("src/x.rs")));
        assert!(!bb.is_focused_dirty(Path::new("src/y.rs")));
        assert!(!bb.is_focused_dirty(Path::new("src/z.rs")));
        assert_eq!(
            bb.buffer_bytes(Path::new("src/x.rs")),
            Some(&b"unsaved"[..])
        );
        assert_eq!(bb.buffer_bytes(Path::new("src/z.rs")), None);

        let events = bb.poll();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|e| e.source == ChangeSourceKind::BufferBridge));
    }

    #[test]
    fn rescan_diffs_add_modify_remove_against_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("keep.txt"), b"v1").unwrap();
        fs::write(root.join("gone.txt"), b"old").unwrap();

        let mut rescan = ReconciliationRescan::new(root, matcher());
        // Seed the baseline with the initial state.
        let baseline = rescan.scan_tree().unwrap();
        rescan.set_baseline(baseline);

        // Simulate out-of-band bash mutations with std::fs:
        //   modify keep.txt, add new.txt, rm gone.txt.
        fs::write(root.join("keep.txt"), b"v2-modified").unwrap();
        fs::write(root.join("new.txt"), b"added").unwrap();
        fs::remove_file(root.join("gone.txt")).unwrap();

        rescan.rescan().unwrap();
        let events = rescan.poll();
        let find = |p: &str, op: ChangeOp| {
            events.iter().any(|e| {
                e.path == Path::new(p) && e.op == op && e.source == ChangeSourceKind::Rescan
            })
        };
        assert!(find("keep.txt", ChangeOp::Modified), "{events:?}");
        assert!(find("new.txt", ChangeOp::Added), "{events:?}");
        assert!(find("gone.txt", ChangeOp::Removed), "{events:?}");
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn rescan_first_pass_reports_everything_as_added() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), b"b").unwrap();

        let mut rescan = ReconciliationRescan::new(dir.path(), matcher());
        rescan.rescan().unwrap();
        let events = rescan.poll();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.op == ChangeOp::Added));
    }

    #[test]
    fn rescan_honors_the_ignore_profile() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules/dep.js"), b"x").unwrap();
        fs::write(dir.path().join("real.rs"), b"y").unwrap();

        let rescan = ReconciliationRescan::new(dir.path(), matcher());
        let scan = rescan.scan_tree().unwrap();
        assert!(scan.contains_key(Path::new("real.rs")));
        assert!(!scan.contains_key(Path::new("node_modules/dep.js")));
    }
}
