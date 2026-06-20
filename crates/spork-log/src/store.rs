//! The log handle: [`EventLog`].
//!
//! An `EventLog` owns the path to the SQLite-WAL database and the single writer
//! actor. It hands out write access through one cloneable [`WriterHandle`] (the
//! only write path) and read access through independent [`Reader`]s (one
//! connection each, any number concurrent). This mirrors DESIGN.md §5.2's
//! single-writer / many-readers topology.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use spork_migrate::MigrationRegistry;

use crate::error::Result;
use crate::reader::{CurrentVersions, Reader};
use crate::writer::{WriterActor, WriterHandle};

/// An open append-only, hash-chained event log.
///
/// Construct with [`EventLog::open`]. The log creates its schema on first open,
/// runs SQLite in WAL mode with `synchronous=FULL` durability, and spawns the
/// single writer actor that exclusively owns the write connection. Drop the
/// `EventLog` to shut the writer down (it drains then joins).
pub struct EventLog {
    path: PathBuf,
    writer: WriterActor,
}

impl EventLog {
    /// Open (creating if absent) the event log at `path`.
    ///
    /// Sets up the WAL journal mode and `synchronous=FULL`, creates the `event`
    /// table if it does not exist, spawns the writer actor (which loads the chain
    /// tip so a reopened log continues the existing chain), and returns a handle.
    ///
    /// # Errors
    ///
    /// - [`crate::LogError::Sqlite`] if the database cannot be opened, the
    ///   pragmas cannot be applied, or the schema cannot be created.
    /// - [`crate::LogError::NotContiguous`] if an existing database has a
    ///   non-contiguous stored sequence (corruption).
    pub fn open(path: &Path) -> Result<EventLog> {
        let path = path.to_path_buf();
        let writer = WriterActor::spawn(path.clone())?;
        Ok(EventLog { path, writer })
    }

    /// Return a handle to the single writer actor — the only write path.
    ///
    /// All clones funnel into the same actor, so writes are strictly serialized
    /// no matter how many handles or threads call [`WriterHandle::append`].
    pub fn writer(&self) -> WriterHandle {
        self.writer.handle()
    }

    /// Open a fresh read-only reader with its own connection.
    ///
    /// Any number of readers can run concurrently with the writer and with each
    /// other under WAL.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LogError::Sqlite`] if a read-only connection cannot be
    /// opened.
    pub fn reader(&self) -> Result<Reader> {
        Reader::open(&self.path)
    }

    /// Open a reader that lazily upgrades older payloads on read (CLAUDE.md C5).
    ///
    /// `registry` supplies forward migrations and `current_versions` maps each
    /// event type to the version the caller treats as current. Reading an event
    /// stored below its current version upgrades the in-memory payload through
    /// the registry; storage is never rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LogError::Sqlite`] if a read-only connection cannot be
    /// opened.
    pub fn reader_with_migrations(
        &self,
        registry: Arc<MigrationRegistry>,
        current_versions: CurrentVersions,
    ) -> Result<Reader> {
        Reader::open_with_migrations(&self.path, registry, Arc::new(current_versions))
    }

    /// The filesystem path of the underlying database.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
