//! TTL leases, the durable lease ledger, and the crash-safe reaper.
//!
//! Every workspace and resource is acquired under a TTL **lease**; a crash-safe
//! **reaper** reclaims anything whose lease expired or whose owning process died
//! — via a *durable* lease ledger — guaranteeing teardown even on an IDE crash
//! (DESIGN.md §11.3). Long-running stress tests renew via heartbeat so the
//! reaper does not kill them prematurely.
//!
//! The pieces:
//!
//! - [`Lease`] — the lease record: a ULID id, the owning process pid, the TTL
//!   and heartbeat intervals, a durability flag, and the timestamp of the last
//!   heartbeat. It carries its own [`schema_version`](Lease::schema_version)
//!   (constraint C5) so the durable record's shape can evolve.
//! - [`LeaseLedger`] — a **durable**, append-and-replace JSON ledger on disk.
//!   It survives a restart: reopening the ledger reads back every recorded
//!   lease, which is what lets the reaper reclaim a workspace whose owner died
//!   *before* this process started.
//! - [`Reaper`] — scans the ledger and reclaims every lease that is **expired**
//!   (its TTL elapsed since the last heartbeat) or whose **owner process is
//!   dead** (the pid no longer exists). Reclamation removes the workspace
//!   directory and drops the lease from the ledger; it is idempotent and
//!   reaper-safe with a normal teardown.
//!
//! Liveness is split behind two small seams so the whole thing is
//! deterministically testable offline: a [`Clock`] supplies "now" (a fake clock
//! drives TTL-expiry tests without sleeping), and a [`ProcessChecker`] answers
//! "is this pid alive?" (a fake checker simulates a dead owner across a reopen,
//! which is exactly the crash the reaper must survive). The production
//! [`SystemClock`] and [`SystemProcessChecker`] use the wall clock and a real,
//! signal-0 liveness probe.
//!
//! Design references: DESIGN.md §5.3 (TTL lease + crash-safe reaper), §11.3
//! (lease-based teardown, heartbeat renewal, durable lease ledger).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{ExecError, Result};

/// The current schema version stamped on a freshly minted [`Lease`].
pub const LEASE_VERSION: u16 = 1;

/// The current schema version of the on-disk [`LeaseLedger`] file.
pub const LEDGER_VERSION: u16 = 1;

/// A TTL lease over a workspace or resource (DESIGN.md §11.3).
///
/// A lease authorizes the holder to mutate a workspace; it is held for
/// [`ttl_ms`](Lease::ttl_ms) past its last heartbeat. A long-running run renews
/// the lease by heartbeat (updating [`last_heartbeat_ms`](Lease::last_heartbeat_ms))
/// so the reaper does not reclaim it prematurely. If the holder process dies or
/// the TTL lapses without a heartbeat, the lease is reclaimable. The record is
/// persisted in the durable [`LeaseLedger`] and carries its own
/// [`schema_version`](Lease::schema_version) (constraint C5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// The schema version of this record ([`LEASE_VERSION`] for fresh leases).
    pub schema_version: u16,
    /// The lease id — a ULID, lexicographically sortable and time-ordered, to
    /// match the node/edge identity scheme across F1/F2.
    pub id: Ulid,
    /// The pid of the process that owns (and must renew) this lease. The reaper
    /// reclaims the lease if this pid is no longer alive.
    pub owner_pid: u32,
    /// How long the lease stays valid past its last heartbeat, in milliseconds.
    pub ttl_ms: u64,
    /// The heartbeat interval, in milliseconds (informational: the cadence the
    /// owner is expected to renew at; the reaper keys off `ttl_ms`).
    pub heartbeat_ms: u64,
    /// Whether this lease must be persisted durably so it survives a restart.
    /// All workspace leases are durable so the reaper can reclaim them after a
    /// crash; a transient resource lease may not be.
    pub durable: bool,
    /// The wall-clock time of the last heartbeat, in milliseconds since the Unix
    /// epoch. Set at acquisition and updated on each [`heartbeat`](LeaseLedger::heartbeat).
    pub last_heartbeat_ms: u64,
}

impl Lease {
    /// Whether this lease has expired *as of* `now_ms` — its TTL elapsed since
    /// the last heartbeat.
    #[must_use]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_heartbeat_ms) > self.ttl_ms
    }
}

/// A monotonic-ish source of wall-clock milliseconds, behind a seam so tests can
/// drive TTL expiry deterministically.
pub trait Clock: std::fmt::Debug {
    /// Milliseconds since the Unix epoch "now".
    fn now_ms(&self) -> u64;
}

/// The production [`Clock`]: the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A liveness oracle for process ids, behind a seam so tests can simulate a dead
/// owner without actually killing a process.
pub trait ProcessChecker: std::fmt::Debug {
    /// Whether the process with pid `pid` is currently alive.
    fn is_alive(&self, pid: u32) -> bool;
}

/// The production [`ProcessChecker`]: a real liveness probe.
///
/// On Unix it sends signal 0 to the pid (the standard "does this process exist
/// and may I signal it" probe, which sends no actual signal). On other platforms
/// it conservatively reports the current process alive and others as alive too —
/// the reaper then relies on TTL expiry rather than pid liveness on those
/// platforms.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessChecker;

impl ProcessChecker for SystemProcessChecker {
    fn is_alive(&self, pid: u32) -> bool {
        #[cfg(unix)]
        {
            // `kill(pid, 0)` performs error checking without sending a signal:
            // it returns 0 if the process exists, or ESRCH if it does not. We
            // shell out to `kill -0` to avoid pulling in a libc dependency while
            // keeping `#![forbid(unsafe_code)]`.
            std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            true
        }
    }
}

/// The durable, persisted set of leases, plus the workspace each one owns.
///
/// The ledger is the crash-safety substrate: it is written to disk on every
/// mutation, so reopening it after a restart reads back every recorded lease
/// (and the workspace path it guards). That is what lets the [`Reaper`] reclaim
/// a workspace whose owner died *before* the current process even started — the
/// crash the design must survive (DESIGN.md §11.3).
///
/// The on-disk form is a small self-describing JSON document carrying its own
/// [`LEDGER_VERSION`] (constraint C5).
#[derive(Debug)]
pub struct LeaseLedger {
    path: PathBuf,
    state: LedgerState,
}

/// The serialized ledger document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerState {
    /// Schema version of the on-disk ledger document.
    schema_version: u16,
    /// Every recorded lease, keyed by its textual ULID, with the workspace root
    /// it guards.
    entries: BTreeMap<String, LedgerEntry>,
}

impl Default for LedgerState {
    fn default() -> Self {
        LedgerState {
            schema_version: LEDGER_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

/// One ledger record: a lease and the workspace path it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerEntry {
    lease: Lease,
    workspace_root: PathBuf,
}

impl LeaseLedger {
    /// Open (or create) the durable ledger at `path`.
    ///
    /// If the file exists it is read back — including every lease recorded by a
    /// prior, possibly-crashed process — so the reaper can act on it. A missing
    /// file starts an empty ledger.
    ///
    /// # Errors
    /// I/O errors reading the file, or [`ExecError::Decode`] if the file exists
    /// but does not parse (corruption, or a future schema version).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let state = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| ExecError::io(&path, e))?;
            let state: LedgerState =
                serde_json::from_slice(&bytes).map_err(|e| ExecError::Decode(e.to_string()))?;
            if state.schema_version != LEDGER_VERSION {
                return Err(ExecError::Decode(format!(
                    "unsupported lease ledger schema version {} (this build expects {LEDGER_VERSION})",
                    state.schema_version
                )));
            }
            state
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| ExecError::io(parent, e))?;
            }
            LedgerState::default()
        };
        Ok(LeaseLedger { path, state })
    }

    /// Acquire a new durable lease for `workspace_root`, owned by `owner_pid`,
    /// with the given TTL and heartbeat intervals.
    ///
    /// The lease is persisted to disk before returning, so a crash immediately
    /// after acquisition still leaves a recoverable record for the reaper.
    ///
    /// # Errors
    /// I/O errors persisting the ledger.
    pub fn acquire(
        &mut self,
        workspace_root: impl Into<PathBuf>,
        owner_pid: u32,
        ttl_ms: u64,
        heartbeat_ms: u64,
        clock: &dyn Clock,
    ) -> Result<Lease> {
        let lease = Lease {
            schema_version: LEASE_VERSION,
            id: Ulid::new(),
            owner_pid,
            ttl_ms,
            heartbeat_ms,
            durable: true,
            last_heartbeat_ms: clock.now_ms(),
        };
        self.state.entries.insert(
            lease.id.to_string(),
            LedgerEntry {
                lease: lease.clone(),
                workspace_root: workspace_root.into(),
            },
        );
        self.persist()?;
        Ok(lease)
    }

    /// Renew a lease by recording a fresh heartbeat at `clock`'s now.
    ///
    /// A long-running run calls this within each TTL window so the reaper does
    /// not reclaim it (DESIGN.md §11.3).
    ///
    /// # Errors
    /// [`ExecError::UnknownLease`] if no such lease is recorded, or I/O errors
    /// persisting.
    pub fn heartbeat(&mut self, lease_id: &Ulid, clock: &dyn Clock) -> Result<()> {
        let key = lease_id.to_string();
        let entry = self
            .state
            .entries
            .get_mut(&key)
            .ok_or_else(|| ExecError::UnknownLease(key.clone()))?;
        entry.lease.last_heartbeat_ms = clock.now_ms();
        self.persist()
    }

    /// Whether a lease with `lease_id` is currently held and valid as of
    /// `clock`'s now (present, not expired).
    ///
    /// Note this checks only TTL, not owner liveness — owner-death reclamation
    /// is the [`Reaper`]'s job. A live caller asserting its own lease is, by
    /// definition, alive.
    #[must_use]
    pub fn is_held(&self, lease_id: &Ulid, clock: &dyn Clock) -> bool {
        self.state
            .entries
            .get(&lease_id.to_string())
            .is_some_and(|e| !e.lease.is_expired(clock.now_ms()))
    }

    /// Release a lease explicitly, dropping it from the ledger (used on a normal
    /// teardown). A lease that is already absent is a no-op success.
    ///
    /// # Errors
    /// I/O errors persisting the ledger.
    pub fn release(&mut self, lease_id: &Ulid) -> Result<()> {
        if self.state.entries.remove(&lease_id.to_string()).is_some() {
            self.persist()?;
        }
        Ok(())
    }

    /// The workspace root guarded by a lease, if recorded.
    #[must_use]
    pub fn workspace_root(&self, lease_id: &Ulid) -> Option<PathBuf> {
        self.state
            .entries
            .get(&lease_id.to_string())
            .map(|e| e.workspace_root.clone())
    }

    /// The number of leases currently recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.entries.len()
    }

    /// Whether the ledger holds no leases.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.entries.is_empty()
    }

    /// The lease records that are reclaimable as of `clock`'s now: those whose
    /// TTL has expired, or whose owner process is no longer alive.
    fn reclaimable(
        &self,
        clock: &dyn Clock,
        checker: &dyn ProcessChecker,
    ) -> Vec<(Ulid, PathBuf, ReclaimReason)> {
        let now = clock.now_ms();
        let mut out = Vec::new();
        for entry in self.state.entries.values() {
            let reason = if !checker.is_alive(entry.lease.owner_pid) {
                Some(ReclaimReason::OwnerDead {
                    pid: entry.lease.owner_pid,
                })
            } else if entry.lease.is_expired(now) {
                Some(ReclaimReason::Expired {
                    ttl_ms: entry.lease.ttl_ms,
                })
            } else {
                None
            };
            if let Some(reason) = reason {
                out.push((entry.lease.id, entry.workspace_root.clone(), reason));
            }
        }
        out
    }

    /// Atomically persist the ledger to disk (write to a temp file then rename),
    /// so a crash mid-write never leaves a half-written ledger.
    fn persist(&self) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(&self.state).map_err(|e| ExecError::Decode(e.to_string()))?;
        let tmp = self.path.with_extension("ledger.tmp");
        fs::write(&tmp, &bytes).map_err(|e| ExecError::io(&tmp, e))?;
        fs::rename(&tmp, &self.path).map_err(|e| ExecError::io(&self.path, e))?;
        Ok(())
    }
}

/// Why a lease was reclaimed by the [`Reaper`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimReason {
    /// The lease's TTL elapsed since its last heartbeat.
    Expired {
        /// The TTL that elapsed, in milliseconds.
        ttl_ms: u64,
    },
    /// The lease's owning process is no longer alive.
    OwnerDead {
        /// The dead owner's pid.
        pid: u32,
    },
}

impl ReclaimReason {
    /// A short, stable label for diagnostics.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            ReclaimReason::Expired { .. } => "expired",
            ReclaimReason::OwnerDead { .. } => "owner-dead",
        }
    }
}

/// A record of one reclaimed workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reclaimed {
    /// The lease id that was reclaimed.
    pub lease_id: Ulid,
    /// The workspace root that was removed.
    pub workspace_root: PathBuf,
    /// Why it was reclaimed.
    pub reason: ReclaimReason,
}

/// The crash-safe reaper (DESIGN.md §5.3, §11.3).
///
/// A `Reaper` reclaims every workspace whose lease has expired or whose owner
/// process is dead, removing the workspace directory and dropping the lease from
/// the durable ledger. Because it reads the *durable* ledger, it works across a
/// restart: a workspace orphaned by a crashed process is reclaimed the next time
/// the reaper runs, even though this process never held the lease. Reclamation
/// is idempotent and converges with a normal teardown on the same end state.
///
/// The clock and process-liveness oracle are injected ([`Clock`],
/// [`ProcessChecker`]) so the reaper's behavior is deterministically testable
/// offline.
#[derive(Debug)]
pub struct Reaper<C: Clock, P: ProcessChecker> {
    clock: C,
    checker: P,
}

impl Default for Reaper<SystemClock, SystemProcessChecker> {
    fn default() -> Self {
        Reaper {
            clock: SystemClock,
            checker: SystemProcessChecker,
        }
    }
}

impl<C: Clock, P: ProcessChecker> Reaper<C, P> {
    /// Construct a reaper with the given clock and process-liveness oracle.
    #[must_use]
    pub fn new(clock: C, checker: P) -> Self {
        Reaper { clock, checker }
    }

    /// Scan `ledger` and reclaim every expired or dead-owner lease, removing the
    /// workspace directory and dropping the lease.
    ///
    /// Returns the records of what was reclaimed (empty if nothing was). A
    /// workspace directory that is already gone is treated as success — the
    /// goal is convergence, not a guarantee the reaper was the one to delete it.
    ///
    /// # Errors
    /// I/O errors removing a workspace directory (other than "already absent"),
    /// or I/O errors persisting the updated ledger.
    pub fn reap(&self, ledger: &mut LeaseLedger) -> Result<Vec<Reclaimed>> {
        let targets = ledger.reclaimable(&self.clock, &self.checker);
        let mut reclaimed = Vec::with_capacity(targets.len());
        for (lease_id, root, reason) in targets {
            remove_workspace(&root)?;
            ledger.release(&lease_id)?;
            reclaimed.push(Reclaimed {
                lease_id,
                workspace_root: root,
                reason,
            });
        }
        Ok(reclaimed)
    }
}

/// Remove a workspace directory, treating "already absent" as success.
pub(crate) fn remove_workspace(root: &Path) -> Result<()> {
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ExecError::io(root, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashSet;
    use tempfile::TempDir;

    /// A fake clock whose "now" can be set, for deterministic TTL tests.
    #[derive(Debug)]
    struct FakeClock {
        now: Cell<u64>,
    }
    impl FakeClock {
        fn new(now: u64) -> Self {
            FakeClock {
                now: Cell::new(now),
            }
        }
        fn set(&self, now: u64) {
            self.now.set(now);
        }
    }
    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
    }

    /// A fake process checker with a fixed set of "alive" pids.
    #[derive(Debug)]
    struct FakeChecker {
        alive: HashSet<u32>,
    }
    impl FakeChecker {
        fn with_alive(pids: impl IntoIterator<Item = u32>) -> Self {
            FakeChecker {
                alive: pids.into_iter().collect(),
            }
        }
        fn none_alive() -> Self {
            FakeChecker {
                alive: HashSet::new(),
            }
        }
    }
    impl ProcessChecker for FakeChecker {
        fn is_alive(&self, pid: u32) -> bool {
            self.alive.contains(&pid)
        }
    }

    fn make_workspace(tmp: &TempDir, name: &str) -> PathBuf {
        let p = tmp.path().join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("file.txt"), b"content").unwrap();
        p
    }

    #[test]
    fn acquire_persists_and_is_held() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(1_000);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 42, 5_000, 1_000, &clock).unwrap();

        assert!(ledger.is_held(&lease.id, &clock));
        assert_eq!(ledger.workspace_root(&lease.id), Some(ws));
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn lease_expires_after_ttl_without_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(0);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 42, 5_000, 1_000, &clock).unwrap();

        clock.set(4_000);
        assert!(ledger.is_held(&lease.id, &clock), "within ttl still held");
        clock.set(6_000);
        assert!(!ledger.is_held(&lease.id, &clock), "past ttl expired");
    }

    #[test]
    fn heartbeat_extends_the_lease() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(0);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 42, 5_000, 1_000, &clock).unwrap();

        clock.set(4_000);
        ledger.heartbeat(&lease.id, &clock).unwrap();
        clock.set(8_000); // 4s since heartbeat, still < 5s ttl
        assert!(ledger.is_held(&lease.id, &clock));
    }

    #[test]
    fn reaper_reclaims_an_expired_lease() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(0);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 42, 5_000, 1_000, &clock).unwrap();

        clock.set(10_000); // well past ttl
        let reaper = Reaper::new(FakeClock::new(10_000), FakeChecker::with_alive([42]));
        let reclaimed = reaper.reap(&mut ledger).unwrap();

        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].lease_id, lease.id);
        assert_eq!(
            reclaimed[0].reason,
            ReclaimReason::Expired { ttl_ms: 5_000 }
        );
        assert!(!ws.exists(), "workspace removed");
        assert!(ledger.is_empty(), "lease dropped from ledger");
    }

    #[test]
    fn dead_owner_lease_is_reclaimed_across_a_reopen() {
        // The crash the design must survive: a process acquires a workspace
        // lease, then dies. A *new* process opens the durable ledger and runs
        // the reaper; the orphaned workspace is reclaimed even though the new
        // process never held the lease and the lease's TTL has not yet elapsed.
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "orphan");
        let ledger_path = tmp.path().join("leases.json");

        // --- "old" process: acquire and then crash (drop the ledger). ---
        let dead_pid = {
            let clock = FakeClock::new(1_000);
            let mut ledger = LeaseLedger::open(&ledger_path).unwrap();
            let lease = ledger.acquire(&ws, 99_999, 60_000, 5_000, &clock).unwrap();
            assert!(ledger.is_held(&lease.id, &clock));
            // ledger dropped here = "process exited"; the durable file remains.
            99_999u32
        };
        assert!(ws.exists());

        // --- "new" process: reopen the durable ledger after the crash. ---
        let mut reopened = LeaseLedger::open(&ledger_path).unwrap();
        assert_eq!(reopened.len(), 1, "lease survived the restart");

        // Only a tiny amount of wall time has passed (TTL is 60s, not elapsed),
        // so the *only* reason this is reclaimable is that the owner pid is dead.
        let reaper = Reaper::new(
            FakeClock::new(2_000),
            FakeChecker::none_alive(), // owner 99999 is dead
        );
        let reclaimed = reaper.reap(&mut reopened).unwrap();

        assert_eq!(reclaimed.len(), 1);
        assert_eq!(
            reclaimed[0].reason,
            ReclaimReason::OwnerDead { pid: dead_pid }
        );
        assert!(!ws.exists(), "orphaned workspace reclaimed on restart");
        assert!(reopened.is_empty());

        // And the durable ledger on disk now reflects the reclamation.
        let after = LeaseLedger::open(&ledger_path).unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn live_owner_within_ttl_is_not_reaped() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(1_000);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 7, 60_000, 5_000, &clock).unwrap();

        let reaper = Reaper::new(FakeClock::new(2_000), FakeChecker::with_alive([7]));
        let reclaimed = reaper.reap(&mut ledger).unwrap();
        assert!(reclaimed.is_empty(), "live owner within ttl survives");
        assert!(ws.exists());
        assert!(ledger.is_held(&lease.id, &clock));
    }

    #[test]
    fn release_drops_a_lease() {
        let tmp = TempDir::new().unwrap();
        let ws = make_workspace(&tmp, "ws");
        let clock = FakeClock::new(0);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let lease = ledger.acquire(&ws, 1, 5_000, 1_000, &clock).unwrap();
        ledger.release(&lease.id).unwrap();
        assert!(ledger.is_empty());
        // Releasing again is a no-op success.
        ledger.release(&lease.id).unwrap();
    }

    #[test]
    fn heartbeat_unknown_lease_errors() {
        let tmp = TempDir::new().unwrap();
        let clock = FakeClock::new(0);
        let mut ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
        let err = ledger.heartbeat(&Ulid::new(), &clock).unwrap_err();
        assert!(matches!(err, ExecError::UnknownLease(_)));
    }

    #[test]
    fn ledger_rejects_a_future_schema_version() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("leases.json");
        let doc = serde_json::json!({
            "schema_version": LEDGER_VERSION + 1,
            "entries": {}
        });
        fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let err = LeaseLedger::open(&path).unwrap_err();
        assert!(matches!(err, ExecError::Decode(_)));
    }

    #[test]
    fn lease_round_trips_through_serde() {
        let clock = FakeClock::new(1234);
        let tmp = TempDir::new().unwrap();
        let mut ledger = LeaseLedger::open(tmp.path().join("l.json")).unwrap();
        let ws = make_workspace(&tmp, "ws");
        let lease = ledger.acquire(&ws, 5, 1000, 500, &clock).unwrap();
        let json = serde_json::to_string(&lease).unwrap();
        let back: Lease = serde_json::from_str(&json).unwrap();
        assert_eq!(lease, back);
    }
}
