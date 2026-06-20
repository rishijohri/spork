//! Read-only access to the log: [`Reader`].
//!
//! Readers open their **own** read-only SQLite connection, so any number can run
//! concurrently with the single writer under WAL (DESIGN.md §5.2). A reader
//! offers point lookups ([`Reader::get`]), ordered iteration
//! ([`Reader::iter_from`]), length ([`Reader::len`]), full hash-chain
//! verification ([`Reader::verify_chain`]), and **lazy upgrade-on-read** of older
//! payloads via the migration registry — without ever rewriting storage
//! (CLAUDE.md C5, DESIGN.md §6.1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use spork_migrate::MigrationRegistry;

use crate::error::{LogError, Result};
use crate::event::{Event, GENESIS_PREV_HASH};
use crate::schema;

/// A resolver from event type to its current schema version.
///
/// When a [`Reader`] is configured with migrations, an event whose stored
/// `schema_version` is *below* the current version for its type is upgraded on
/// read. The map holds the current version per type; an unmapped type is read
/// as-is (its stored version is already current by definition).
pub type CurrentVersions = HashMap<String, u16>;

/// Optional migration configuration carried by a [`Reader`].
#[derive(Clone)]
struct Migrations {
    registry: Arc<MigrationRegistry>,
    current: Arc<CurrentVersions>,
}

/// Read-only access to an event log.
///
/// Each `Reader` owns an independent read-only connection. Cloning is *not*
/// provided because a `rusqlite::Connection` is not `Sync`; instead, call
/// [`crate::EventLog::reader`] again to get another concurrent reader (each on
/// its own connection).
pub struct Reader {
    conn: Connection,
    path: PathBuf,
    migrations: Option<Migrations>,
}

impl Reader {
    /// Open a plain reader (no migration-on-read) on `path`.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Sqlite`] if the database cannot be opened read-only.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Ok(Reader {
            conn: schema::open_reader(path)?,
            path: path.to_path_buf(),
            migrations: None,
        })
    }

    /// Open a reader that lazily upgrades older payloads on read.
    ///
    /// `registry` supplies the forward migrations; `current` maps each event
    /// type to the schema version the caller considers current. An event read at
    /// a stored version below its current version has its payload upgraded
    /// through the registry; the stored bytes are never touched.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Sqlite`] if the database cannot be opened read-only.
    pub(crate) fn open_with_migrations(
        path: &Path,
        registry: Arc<MigrationRegistry>,
        current: Arc<CurrentVersions>,
    ) -> Result<Self> {
        Ok(Reader {
            conn: schema::open_reader(path)?,
            path: path.to_path_buf(),
            migrations: Some(Migrations { registry, current }),
        })
    }

    /// The number of events in the log (equivalently, the last `seq`).
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Sqlite`] on a query failure.
    pub fn len(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
        Ok(count as u64)
    }

    /// Whether the log has no events.
    ///
    /// # Errors
    ///
    /// See [`Reader::len`].
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Fetch the event at `seq`, or `None` if it does not exist.
    ///
    /// If the reader was opened with migrations, the returned event's payload is
    /// upgraded to its current schema version (its `schema_version` field is set
    /// to the upgraded-to version).
    ///
    /// # Errors
    ///
    /// - [`LogError::Sqlite`] on a query/decode failure.
    /// - [`LogError::Migration`] if a configured upgrade fails.
    /// - [`LogError::Canon`] only via an upgrade path that re-canonicalizes.
    pub fn get(&self, seq: u64) -> Result<Option<Event>> {
        let sql = format!("SELECT {} FROM event WHERE seq = ?1", schema::COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([seq as i64])?;
        match rows.next()? {
            Some(row) => {
                let event = schema::row_to_event(row)?;
                Ok(Some(self.maybe_upgrade(event)?))
            }
            None => Ok(None),
        }
    }

    /// Iterate events with `seq >= start_seq`, in ascending `seq` order.
    ///
    /// The iterator is *materialized eagerly* into an owned vector before being
    /// returned, which keeps the public signature free of a borrow on `self`
    /// (and avoids lending out a live `rusqlite` statement). Each item is a
    /// `Result`: a decode/upgrade failure surfaces as an `Err` element rather
    /// than aborting iteration. If the reader has migrations, each event is
    /// upgraded on the way out.
    ///
    /// # Errors
    ///
    /// The returned iterator yields [`LogError`] elements on decode/upgrade
    /// failure; the call itself returns [`LogError::Sqlite`] only if the initial
    /// query cannot be prepared/executed.
    pub fn iter_from(&self, start_seq: u64) -> Result<impl Iterator<Item = Result<Event>>> {
        let sql = format!(
            "SELECT {} FROM event WHERE seq >= ?1 ORDER BY seq ASC",
            schema::COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([start_seq as i64], |row| {
            // Decode inside the closure but defer error mapping: `query_map`
            // needs `rusqlite::Error`, so we capture our richer result in an
            // `Ok`-wrapped inner result and flatten below.
            Ok(schema::row_to_event(row))
        })?;

        let mut out: Vec<Result<Event>> = Vec::new();
        for r in rows {
            // Outer `r` is the row-fetch result; inner is our decode result.
            match r {
                Ok(Ok(event)) => out.push(self.maybe_upgrade(event)),
                Ok(Err(e)) => out.push(Err(e)),
                Err(e) => out.push(Err(LogError::from(e))),
            }
        }
        Ok(out.into_iter())
    }

    /// Recompute the entire hash chain and verify every link.
    ///
    /// For each event in ascending `seq` order this checks two things:
    /// 1. the event's `prev_event_hash` equals the previous event's
    ///    `this_event_hash` (or [`GENESIS_PREV_HASH`] for `seq == 1`), and
    /// 2. recomputing `this_event_hash` from the stored fields reproduces the
    ///    stored value.
    ///
    /// It also enforces that `seq` is contiguous from 1. Any failure returns the
    /// `seq` at which it was detected, which is the tamper/corruption point.
    ///
    /// Verification always reads the **stored** (un-migrated) payload, because
    /// the chain commits to the bytes as written; an upgraded payload would not
    /// reproduce the original hash. This method therefore ignores any migration
    /// configuration.
    ///
    /// # Errors
    ///
    /// - [`LogError::HashChainBroken`] at the first bad link.
    /// - [`LogError::NotContiguous`] if a `seq` gap is found.
    /// - [`LogError::Sqlite`] / [`LogError::Canon`] on decode/canon failure.
    pub fn verify_chain(&self) -> Result<()> {
        let sql = format!("SELECT {} FROM event ORDER BY seq ASC", schema::COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;

        let mut expected_seq: u64 = 1;
        let mut expected_prev = GENESIS_PREV_HASH;

        while let Some(row) = rows.next()? {
            let event = schema::row_to_event(row)?;

            if event.seq != expected_seq {
                return Err(LogError::NotContiguous {
                    expected: expected_seq,
                    got: event.seq,
                });
            }
            // Link check: this event must chain from the previous tip.
            if event.prev_event_hash != expected_prev {
                return Err(LogError::HashChainBroken { seq: event.seq });
            }
            // Self check: stored hash must match recomputed hash.
            event.verify_self()?;

            expected_prev = event.this_event_hash;
            expected_seq += 1;
        }
        Ok(())
    }

    /// The path this reader was opened on (useful for diagnostics/tests).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply migration-on-read to a freshly-decoded event, if configured.
    ///
    /// An event whose stored version is below the configured current version for
    /// its type is upgraded through the registry; the returned event reflects the
    /// upgraded payload and version. With no migration config, or for a type not
    /// in the current-versions map, the event is returned unchanged.
    fn maybe_upgrade(&self, event: Event) -> Result<Event> {
        let Some(m) = &self.migrations else {
            return Ok(event);
        };
        let Some(&current) = m.current.get(&event.event_type) else {
            return Ok(event);
        };
        if event.schema_version >= current {
            return Ok(event);
        }
        let (payload, version) = m.registry.upgrade_to_current(
            &event.event_type,
            event.schema_version,
            current,
            event.payload,
        )?;
        Ok(Event {
            payload,
            schema_version: version,
            ..event
        })
    }
}
