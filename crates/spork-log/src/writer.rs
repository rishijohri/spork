//! The single serializing **writer actor** — the only write path.
//!
//! DESIGN.md §5.2: "The single SQLite writer is wrapped in a serializing writer
//! actor that batches event appends so many parallel-branch runners don't
//! bottleneck on it." This module implements that actor as a dedicated thread
//! that *exclusively owns* the write [`Connection`] plus the in-memory chain tip
//! (`last_seq`, `last_hash`). All appends arrive over an `mpsc` channel, so they
//! are linearized by construction: there is no lock to forget and no second
//! writer can exist.
//!
//! Durability: each append is committed under `synchronous=FULL` (an fsync)
//! *before* the actor replies, so `append()` returning `Ok` means the event is
//! durable on disk — the write-objects-then-log guarantee the projection and
//! restore depend on (A.6).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use rusqlite::Connection;
use ulid::Ulid;

use crate::error::{LogError, Result};
use crate::event::{compute_event_hash, Event, NewEvent, GENESIS_PREV_HASH};
use crate::schema;

/// A command sent to the writer actor.
enum Command {
    /// Append an event; reply with the fully-formed, persisted [`Event`].
    Append {
        event: Box<NewEvent>,
        reply: Sender<Result<Event>>,
    },
    /// Report the current length (last assigned `seq`).
    Len { reply: Sender<Result<u64>> },
    /// Stop the actor loop deterministically.
    ///
    /// Sent only by [`WriterActor::drop`]. It ends the loop immediately even if
    /// outstanding cloned [`WriterHandle`]s still hold senders, so dropping the
    /// owning [`crate::EventLog`] never blocks waiting for handle clones to be
    /// released. After the actor exits, those clones' sends fail with
    /// [`writer_gone`], which is the correct "the log is closed" signal.
    Shutdown,
}

/// A cloneable handle to the single writer actor.
///
/// Cloning a handle does **not** create a second writer — every clone funnels
/// into the same actor thread and the same connection, so writes stay strictly
/// serialized. The actor's lifetime is owned by the [`crate::EventLog`]: when the
/// log is dropped, the actor is told to shut down and is joined, *regardless* of
/// how many handle clones are still outstanding. A call on a handle after the log
/// has been dropped fails with a "writer is no longer running" error rather than
/// hanging.
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<Command>,
}

impl WriterHandle {
    /// Append `e` to the log, returning the fully-formed persisted [`Event`].
    ///
    /// The actor assigns `seq = last + 1`, sets `prev_event_hash` to the
    /// previous event's `this_event_hash` (or [`GENESIS_PREV_HASH`] for the
    /// first event), mints a fresh ULID `event_id`, computes `this_event_hash`
    /// over the frozen preimage, and commits the row durably before returning.
    /// Concurrent calls from multiple [`WriterHandle`] clones are linearized by
    /// the actor's single inbox.
    ///
    /// # Errors
    ///
    /// - [`LogError::Canon`] if the payload cannot be canonicalized.
    /// - [`LogError::Sqlite`] on a database/commit failure, or if the writer
    ///   actor thread is no longer running.
    pub fn append(&self, e: NewEvent) -> Result<Event> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Command::Append {
                event: Box::new(e),
                reply,
            })
            .map_err(|_| writer_gone())?;
        rx.recv().map_err(|_| writer_gone())?
    }

    /// Return the number of events currently in the log (the last `seq`).
    ///
    /// This is answered by the actor itself (rather than a reader connection) so
    /// it reflects the authoritative in-memory chain tip even if a checkpoint is
    /// in flight.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Sqlite`] if the writer actor thread is no longer
    /// running.
    pub fn len(&self) -> Result<u64> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Command::Len { reply })
            .map_err(|_| writer_gone())?;
        rx.recv().map_err(|_| writer_gone())?
    }

    /// Whether the log is empty.
    ///
    /// # Errors
    ///
    /// See [`WriterHandle::len`].
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// The actor's owned mutable state: the write connection and the chain tip.
struct WriterState {
    conn: Connection,
    /// The last assigned sequence number (0 when the log is empty).
    last_seq: u64,
    /// The `this_event_hash` of the last event ([`GENESIS_PREV_HASH`] when
    /// empty), i.e. the `prev_event_hash` the next append will chain from.
    last_hash: spork_hash::Hash,
}

impl WriterState {
    /// Append one event durably, advancing the in-memory tip.
    fn append(&mut self, ne: NewEvent) -> Result<Event> {
        let seq = self.last_seq + 1;
        let prev_event_hash = self.last_hash;
        let this_event_hash = compute_event_hash(
            &prev_event_hash,
            &ne.payload,
            seq,
            &ne.event_type,
            ne.schema_version,
        )?;
        let event_id = Ulid::new();

        // Store the payload as canonical bytes so the on-disk BLOB is exactly the
        // bytes the hash chain commits to (and is reproducible across machines).
        let payload_canon = spork_canon::canonicalize_value(&ne.payload)?;

        self.conn.execute(
            "INSERT INTO event \
             (event_id, seq, event_type, schema_version, payload, prev_event_hash, this_event_hash, actor) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                event_id.to_string(),
                seq as i64,
                ne.event_type,
                ne.schema_version as i64,
                payload_canon,
                prev_event_hash.as_bytes().as_slice(),
                this_event_hash.as_bytes().as_slice(),
                ne.actor,
            ],
        )?;

        // The INSERT auto-committed under synchronous=FULL (an fsync), so the
        // row is durable now — only after that do we advance the in-memory tip.
        self.last_seq = seq;
        self.last_hash = this_event_hash;

        Ok(Event {
            event_id,
            seq,
            event_type: ne.event_type,
            schema_version: ne.schema_version,
            payload: ne.payload,
            prev_event_hash,
            this_event_hash,
            actor: ne.actor,
        })
    }
}

/// Read the chain tip (`last_seq`, `last_hash`) from an existing database so a
/// reopened log continues the chain from where it left off.
///
/// Also validates that the stored sequence is contiguous from 1 (the count must
/// equal the maximum `seq`), surfacing corruption as [`LogError::NotContiguous`]
/// rather than silently mis-chaining the next append.
fn load_tip(conn: &Connection) -> Result<(u64, spork_hash::Hash)> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    if count == 0 {
        return Ok((0, GENESIS_PREV_HASH));
    }
    let max_seq: i64 = conn.query_row("SELECT MAX(seq) FROM event", [], |r| r.get(0))?;
    if max_seq != count {
        // A gap between the row count and the max seq means the contiguous-from-1
        // invariant is violated.
        return Err(LogError::NotContiguous {
            expected: count as u64,
            got: max_seq as u64,
        });
    }
    let last_hash_blob: Vec<u8> = conn.query_row(
        "SELECT this_event_hash FROM event WHERE seq = ?1",
        [max_seq],
        |r| r.get(0),
    )?;
    let arr: [u8; spork_hash::HASH_LEN] = last_hash_blob.try_into().map_err(|v: Vec<u8>| {
        LogError::Sqlite(format!(
            "tip this_event_hash is {} bytes, expected {}",
            v.len(),
            spork_hash::HASH_LEN
        ))
    })?;
    Ok((max_seq as u64, spork_hash::Hash::from_bytes(arr)))
}

/// The owning side of the writer actor: holds the join handle and the sender.
///
/// Lives inside [`crate::EventLog`]. Dropping it drops the only retained sender;
/// once every cloned [`WriterHandle`] is also dropped the actor's `recv` returns
/// `Err` and the thread exits, which [`WriterActor::drop`] then joins.
pub struct WriterActor {
    tx: Option<Sender<Command>>,
    handle: Option<JoinHandle<()>>,
}

impl WriterActor {
    /// Spawn the writer actor thread, which opens its own exclusive write
    /// connection to `path` and loads the chain tip.
    ///
    /// Opening happens *on the actor thread* so the write [`Connection`] is never
    /// touched by any other thread — `Connection` is not `Sync`, and confining it
    /// to one thread is what makes "single writer" a compile-time-adjacent fact.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Sqlite`] / [`LogError::NotContiguous`] if the actor
    /// thread fails to open the connection or load a consistent tip.
    pub fn spawn(path: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Command>();
        // A one-shot channel to surface the actor's initialization result back
        // to the caller (open + tip load can fail).
        let (init_tx, init_rx) = mpsc::channel::<Result<()>>();

        let handle = std::thread::Builder::new()
            .name("spork-log-writer".to_string())
            .spawn(move || actor_loop(path, rx, init_tx))
            .map_err(|e| LogError::Sqlite(format!("failed to spawn writer thread: {e}")))?;

        // Wait for the actor to report whether it initialized successfully.
        match init_rx.recv() {
            Ok(Ok(())) => Ok(WriterActor {
                tx: Some(tx),
                handle: Some(handle),
            }),
            Ok(Err(e)) => {
                // The actor reported a failure and is exiting; join to clean up.
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(LogError::Sqlite(
                    "writer thread exited before initializing".to_string(),
                ))
            }
        }
    }

    /// Mint a fresh cloneable handle to this actor.
    pub fn handle(&self) -> WriterHandle {
        WriterHandle {
            // `tx` is `Some` for the entire lifetime until drop.
            tx: self.tx.clone().expect("writer sender present until drop"),
        }
    }
}

impl Drop for WriterActor {
    fn drop(&mut self) {
        // Tell the actor to stop *now*, even if cloned handles still hold senders
        // — otherwise a still-alive handle clone would keep the `recv` loop alive
        // forever and this join would deadlock.
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Command::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            // Best-effort join; the OS reclaims the thread regardless.
            let _ = handle.join();
        }
    }
}

/// The actor thread body: open the connection, report init, then serve commands
/// one at a time until the channel closes.
fn actor_loop(path: PathBuf, rx: Receiver<Command>, init_tx: Sender<Result<()>>) {
    let mut state = match init_state(&path) {
        Ok(state) => {
            // Report success; if the parent already gave up, just exit.
            if init_tx.send(Ok(())).is_err() {
                return;
            }
            state
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    // Serve commands strictly one at a time — this is the serialization point.
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Append { event, reply } => {
                let result = state.append(*event);
                // If the caller dropped its reply receiver, discard the result.
                let _ = reply.send(result);
            }
            Command::Len { reply } => {
                let _ = reply.send(Ok(state.last_seq));
            }
            Command::Shutdown => break,
        }
    }
}

/// Open the write connection and load the chain tip (runs on the actor thread).
fn init_state(path: &std::path::Path) -> Result<WriterState> {
    let conn = schema::open_writer(path)?;
    let (last_seq, last_hash) = load_tip(&conn)?;
    Ok(WriterState {
        conn,
        last_seq,
        last_hash,
    })
}

/// The error used when the actor thread has gone away (channel closed).
fn writer_gone() -> LogError {
    LogError::Sqlite("writer actor is no longer running".to_string())
}
