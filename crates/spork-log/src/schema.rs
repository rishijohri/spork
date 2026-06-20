//! SQLite schema, pragmas, and row <-> [`Event`] marshaling.
//!
//! The store is **SQLite in WAL mode** (single writer, many concurrent readers),
//! exactly the topology DESIGN.md §5.2 calls for ("a clean fit for one IDE
//! process"). The `event` table is declared `STRICT` so column affinities are
//! enforced rather than coerced, and `seq` is `UNIQUE NOT NULL` so the writer's
//! contiguity invariant is also a database constraint.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, Row};
use spork_hash::{Hash, HASH_LEN};
use ulid::Ulid;

use crate::error::{LogError, Result};
use crate::event::Event;

/// The frozen `CREATE TABLE` for the append-only event log.
///
/// `STRICT` enforces declared types; `seq INTEGER UNIQUE NOT NULL` makes the
/// contiguous-sequence invariant a database constraint; `payload` stores the
/// **canonical** bytes (`spork-canon`), and the two hashes store the raw 32-byte
/// BLAKE3 digests as `BLOB`. The `idx_event_seq` index makes ordered scans and
/// `get(seq)` lookups efficient.
const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS event (
    event_id       TEXT    PRIMARY KEY,
    seq            INTEGER UNIQUE NOT NULL,
    event_type     TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    payload        BLOB    NOT NULL,
    prev_event_hash BLOB   NOT NULL,
    this_event_hash BLOB   NOT NULL,
    actor          TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS idx_event_seq ON event(seq);
";

/// Open a connection in **WAL** mode with **`synchronous=FULL`** durability and
/// create the schema if absent.
///
/// `synchronous=FULL` (not the WAL default of `NORMAL`) is deliberate: an
/// `append()` must be durable — committed and fsynced — before it returns, so
/// the write-objects-then-log ordering the projection and restore rely on holds
/// across a crash (DESIGN.md §5.2, A.6). `busy_timeout` is set so a reader
/// briefly contending with the writer waits rather than erroring.
///
/// This is the **writer** connection. Reader connections open read-only (see
/// [`open_reader`]).
///
/// # Errors
///
/// Returns [`LogError::Sqlite`] if the database cannot be opened, the pragmas
/// cannot be set, or the schema cannot be created.
pub fn open_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL allows one writer with many concurrent readers and is the documented
    // topology (§5.2). It persists on the database file across connections.
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL;", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(LogError::Sqlite(format!(
            "failed to enable WAL journal mode (got {mode:?})"
        )));
    }
    // FULL = fsync on every commit: an append is durable before it returns.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(conn)
}

/// Open a **read-only** connection to an existing log.
///
/// Readers never write, so they open with `SQLITE_OPEN_READ_ONLY` and get their
/// own connection — many can run concurrently with the single writer under WAL.
/// `busy_timeout` lets a reader wait out a brief writer checkpoint instead of
/// failing.
///
/// # Errors
///
/// Returns [`LogError::Sqlite`] if the database cannot be opened read-only (e.g.
/// it does not exist yet).
pub fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// The column list shared by every `SELECT`, in a fixed order matching
/// [`row_to_event`].
pub const COLUMNS: &str =
    "event_id, seq, event_type, schema_version, payload, prev_event_hash, this_event_hash, actor";

/// Decode a 32-byte `BLOB` column into a [`Hash`], rejecting a wrong length.
fn blob_to_hash(blob: Vec<u8>, column: &str) -> Result<Hash> {
    let arr: [u8; HASH_LEN] = blob.try_into().map_err(|v: Vec<u8>| {
        LogError::Sqlite(format!(
            "{column} is {} bytes, expected {HASH_LEN}",
            v.len()
        ))
    })?;
    Ok(Hash::from_bytes(arr))
}

/// Marshal a SQLite [`Row`] (selected with [`COLUMNS`]) into an [`Event`].
///
/// The stored `payload` BLOB is canonical JSON bytes; it is parsed back into a
/// [`serde_json::Value`]. The two hash columns are raw 32-byte digests.
///
/// # Errors
///
/// Returns [`LogError::Sqlite`] if a column has an unexpected type/length or the
/// payload bytes are not valid JSON, or the `event_id` is not a valid ULID.
pub fn row_to_event(row: &Row<'_>) -> Result<Event> {
    let event_id_str: String = row.get(0)?;
    let event_id = Ulid::from_string(&event_id_str)
        .map_err(|e| LogError::Sqlite(format!("invalid event_id ULID {event_id_str:?}: {e}")))?;
    let seq: i64 = row.get(1)?;
    let event_type: String = row.get(2)?;
    let schema_version: i64 = row.get(3)?;
    let payload_bytes: Vec<u8> = row.get(4)?;
    let prev_blob: Vec<u8> = row.get(5)?;
    let this_blob: Vec<u8> = row.get(6)?;
    let actor: String = row.get(7)?;

    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| LogError::Sqlite(format!("payload is not valid JSON at seq {seq}: {e}")))?;

    Ok(Event {
        event_id,
        seq: seq as u64,
        event_type,
        schema_version: u16::try_from(schema_version).map_err(|_| {
            LogError::Sqlite(format!("schema_version {schema_version} out of range"))
        })?,
        payload,
        prev_event_hash: blob_to_hash(prev_blob, "prev_event_hash")?,
        this_event_hash: blob_to_hash(this_blob, "this_event_hash")?,
        actor,
    })
}
