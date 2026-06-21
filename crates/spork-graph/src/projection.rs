//! The SQLite-backed materialized graph projection: [`GraphProjection`].
//!
//! The typed graph is a **pure projection of the F1 event log** (DESIGN §6.1,
//! §6.2): this type owns no truth of its own. It folds the graph events
//! ([`crate::events`]) read through a [`spork_log::Reader`] into three SQLite
//! tables and can always be dropped and rebuilt bit-for-bit from the log
//! ([`rebuild_from_log`](GraphProjection::rebuild_from_log)). The "projection ==
//! log" purity is verified by [`canonical_digest`](GraphProjection::canonical_digest):
//! two projections folded from the same events produce an identical digest.
//!
//! # Hot fields are indexed columns, not JSON (DESIGN §6.2)
//!
//! The `node` table promotes the hot fields `status`, `is_stale`, and
//! `snapshot_hash` to real, **indexed** columns rather than burying them in the
//! payload JSON (which SQLite's JSON1 functions query less efficiently on hot
//! paths). The type-specific payload is stored as an opaque `BLOB` of canonical
//! bytes.
//!
//! # Tables (frozen for the F2 generation, CLAUDE.md C5)
//!
//! * `node(id PK, kind, family, owns_snapshot, branch_id, status [idx],
//!   is_stale [idx], snapshot_hash [idx], lineage_hash, payload_schema_version,
//!   op_log_id, model, cost, stale_since, stale_reason, payload BLOB)`
//! * `edge(from, to, edge_type, PRIMARY KEY(from, to, edge_type))`
//! * `ref(name PK, kind, target)`
//!
//! `stale_since`, `stale_reason`, `model`, and `cost` complete the
//! [`NodeEnvelope`](crate::NodeEnvelope) round-trip; they are not hot-path
//! filters so they are plain (unindexed) columns.
//!
//! # The fold ([`apply`](GraphProjection::apply))
//!
//! `apply` is the single fold step. It is deterministic and idempotent with
//! respect to a given event sequence: replaying the same events from an empty
//! projection always yields the same tables (DESIGN A.6). The command layer
//! never writes these tables directly — it validates then appends to the log,
//! and the projection catches up by folding (DESIGN §6.1).

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use spork_edges::{Adjacency, EdgeType};
use spork_hash::Hash;
use spork_log::{Event, Reader};
use spork_registry::Family;
use spork_status::Lifecycle;
use ulid::Ulid;

use crate::envelope::{CostRecord, NodeEnvelope};
use crate::error::{GraphError, Result};
use crate::events::{
    EdgeAddedPayload, NodeCreatedPayload, NodeStatusChangedPayload, RefCreatedPayload,
    RefMovedPayload, EVENT_EDGE_ADDED, EVENT_NODE_CREATED, EVENT_NODE_STATUS_CHANGED,
    EVENT_REF_CREATED, EVENT_REF_MOVED,
};

/// The frozen schema version of the projection table layout (CLAUDE.md C5).
pub const PROJECTION_SCHEMA_VERSION: u16 = 1;

/// The `CREATE TABLE` statements for the materialized graph.
///
/// `STRICT` enforces declared affinities. Hot-path filter columns (`status`,
/// `is_stale`, `snapshot_hash`) get indexes; `edge` is keyed by the full
/// `(from, to, edge_type)` triple so the same relation between two nodes is
/// idempotent on re-fold.
const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS node (
    id                     TEXT    PRIMARY KEY,
    kind                   TEXT    NOT NULL,
    family                 TEXT    NOT NULL,
    owns_snapshot          INTEGER NOT NULL,
    branch_id              TEXT    NOT NULL,
    status                 TEXT    NOT NULL,
    is_stale               INTEGER NOT NULL,
    stale_since            INTEGER,
    stale_reason           TEXT,
    snapshot_hash          TEXT,
    lineage_hash           TEXT    NOT NULL,
    payload_schema_version INTEGER NOT NULL,
    op_log_id              TEXT    NOT NULL,
    model                  TEXT,
    cost                   TEXT,
    payload                BLOB    NOT NULL,
    seq                    INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS idx_node_status        ON node(status);
CREATE INDEX IF NOT EXISTS idx_node_is_stale      ON node(is_stale);
CREATE INDEX IF NOT EXISTS idx_node_snapshot_hash ON node(snapshot_hash);

CREATE TABLE IF NOT EXISTS edge (
    from_id   TEXT NOT NULL,
    to_id     TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    seq       INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, edge_type)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_edge_from ON edge(from_id);
CREATE INDEX IF NOT EXISTS idx_edge_to   ON edge(to_id);

CREATE TABLE IF NOT EXISTS ref (
    name   TEXT PRIMARY KEY,
    kind   TEXT NOT NULL,
    target TEXT NOT NULL
) STRICT;
";

/// A canonical, comparable summary of the whole projection state.
///
/// This is the "drop-and-rebuild yields an identical projection" witness: two
/// projections folded from the same event sequence serialize to byte-identical
/// canonical bytes and therefore the same
/// [`canonical_digest`](GraphProjection::canonical_digest). Ordered containers
/// ([`BTreeMap`]/sorted vectors) make the serialization stable regardless of
/// SQLite row order or insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionState {
    /// All node envelopes, keyed (and thus ordered) by node id string.
    pub nodes: BTreeMap<String, NodeEnvelope>,
    /// All edges, sorted by `(from, to, edge_type)`.
    pub edges: Vec<EdgeRow>,
    /// All refs, keyed (and thus ordered) by ref name.
    pub refs: BTreeMap<String, RefRow>,
}

/// A single materialized edge row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeRow {
    /// The tail node id.
    pub from: Ulid,
    /// The head node id.
    pub to: Ulid,
    /// The typed relation.
    pub edge_type: EdgeType,
}

/// A single materialized ref row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRow {
    /// The ref kind.
    pub kind: spork_edges::RefKind,
    /// The node the ref points at.
    pub target: Ulid,
}

/// The SQLite-backed materialized graph projection.
///
/// Construct one with [`open_in_memory`](GraphProjection::open_in_memory) or
/// [`open`](GraphProjection::open), then either drive it event-by-event with
/// [`apply`](GraphProjection::apply) or rebuild it wholesale with
/// [`rebuild_from_log`](GraphProjection::rebuild_from_log). Reads
/// ([`get_node`](GraphProjection::get_node),
/// [`node_exists`](GraphProjection::node_exists), the [`Adjacency`] impl) go
/// against the materialized tables.
pub struct GraphProjection {
    conn: Connection,
}

impl GraphProjection {
    /// Open an in-memory projection (its own private SQLite database).
    ///
    /// Used for the common case where the projection is a derived cache rebuilt
    /// from the log on startup and need not be persisted between runs.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] if the database cannot be created or the
    /// schema cannot be applied.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| GraphError::Log(format!("open in-memory projection: {e}")))?;
        Self::init(conn)
    }

    /// Open (creating if absent) a file-backed projection at `path`.
    ///
    /// The projection may share the F1 database file or use its own; its tables
    /// (`node`, `edge`, `ref`) are namespaced separately from the F1 `event`
    /// table either way.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] if the database cannot be opened or the schema
    /// cannot be applied.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| GraphError::Log(format!("open projection at {path:?}: {e}")))?;
        Self::init(conn)
    }

    /// Apply pragmas and create the schema on a fresh connection.
    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(|e| GraphError::Log(format!("set foreign_keys: {e}")))?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| GraphError::Log(format!("create projection schema: {e}")))?;
        Ok(GraphProjection { conn })
    }

    /// Rebuild a fresh in-memory projection by folding the entire log.
    ///
    /// Starts from an empty projection and applies every event in ascending
    /// `seq` order. This is the canonical "drop everything and rebuild from the
    /// single source of truth" path (DESIGN §6.1, A.6): the result depends only
    /// on the log contents. If `reader` was configured with migration-on-read,
    /// each node payload is upgraded before it is folded (CLAUDE.md C5).
    ///
    /// # Errors
    ///
    /// - [`GraphError::Log`] if the log cannot be read or an event fails to
    ///   decode/upgrade.
    /// - [`GraphError::Canon`] if a stored payload cannot be re-canonicalized.
    pub fn rebuild_from_log(reader: &Reader) -> Result<Self> {
        let mut proj = Self::open_in_memory()?;
        for item in reader.iter_from(1)? {
            let event = item?;
            proj.apply(&event)?;
        }
        Ok(proj)
    }

    /// Fold a single event into the projection (the projection's whole fold).
    ///
    /// Non-graph events (any whose `event_type` is not one of the frozen graph
    /// strings) are ignored, so the graph projection coexists with other
    /// projections folding the same log. Graph events mutate the materialized
    /// tables; edge and node-created events are idempotent on re-fold by virtue
    /// of `INSERT OR REPLACE` / the `edge` primary key, which keeps a checkpoint
    /// replay and a from-scratch rebuild identical.
    ///
    /// # Errors
    ///
    /// - [`GraphError::Log`] on a malformed graph payload or a SQLite failure.
    /// - [`GraphError::Canon`] if the stored payload cannot be re-canonicalized.
    pub fn apply(&mut self, event: &Event) -> Result<()> {
        match event.event_type.as_str() {
            EVENT_NODE_CREATED => self.apply_node_created(event),
            EVENT_EDGE_ADDED => self.apply_edge_added(event),
            EVENT_REF_CREATED => self.apply_ref_created(event),
            EVENT_REF_MOVED => self.apply_ref_moved(event),
            EVENT_NODE_STATUS_CHANGED => self.apply_node_status_changed(event),
            // Not a graph event — another projection's concern.
            _ => Ok(()),
        }
    }

    /// Decode a graph payload, mapping a decode failure to [`GraphError::Log`].
    fn decode<T: serde::de::DeserializeOwned>(event: &Event) -> Result<T> {
        serde_json::from_value(event.payload.clone()).map_err(|e| {
            GraphError::Log(format!(
                "malformed {} payload at seq {}: {e}",
                event.event_type, event.seq
            ))
        })
    }

    fn apply_node_created(&mut self, event: &Event) -> Result<()> {
        let p: NodeCreatedPayload = Self::decode(event)?;
        // The family is authoritative: it is the value the command layer resolved
        // from the node-type descriptor and recorded on the event. The projection
        // folds it verbatim so the materialized envelope can never disagree with
        // the registered descriptor — including for custom kinds the projection
        // could not classify from the kind string alone (DESIGN §6.2, §7.1; D-5).
        let family = p.family;
        // The node is born with its type's default lifecycle: mutating
        // (snapshot-owning) nodes are `passed` once the snapshot is captured;
        // every other node starts `pending` (DESIGN §7.1).
        let status = if p.owns_snapshot {
            Lifecycle::Passed
        } else {
            Lifecycle::Pending
        };
        // Store the payload as canonical bytes so the stored BLOB is byte-stable
        // and re-canonicalization on a rebuild reproduces the same digest.
        let payload_canon = spork_canon::canonicalize_value(&p.payload)?;
        // P6: a node may carry a `cost` object in its payload — an agent-run
        // records the priced CostRecord of the model turn that produced it (the
        // companion of the `model` attribution F2 already extracts). Lift it into
        // the materialized `cost` column here, in the projection fold. The cost
        // lives inside the already-canonical payload, so this changes no
        // hash-chained event bytes and re-extraction on a rebuild reproduces the
        // same value (the pure-projection property; DESIGN §5.4, §12.5). Absent or
        // malformed → None, exactly as in F2.
        let cost_json: Option<String> = extract_cost_json(&p.payload);

        self.conn
            .execute(
                "INSERT OR REPLACE INTO node \
                 (id, kind, family, owns_snapshot, branch_id, status, is_stale, stale_since, \
                  stale_reason, snapshot_hash, lineage_hash, payload_schema_version, op_log_id, \
                  model, cost, payload, seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                rusqlite::params![
                    p.node_id.to_string(),
                    p.kind,
                    family_tag(family),
                    p.owns_snapshot as i64,
                    p.branch_id,
                    lifecycle_tag(status),
                    0i64,
                    Option::<i64>::None,
                    Option::<String>::None,
                    p.snapshot_hash.map(|h| h.to_hex()),
                    p.lineage_hash.to_hex(),
                    p.payload_schema_version as i64,
                    event.event_id.to_string(),
                    p.model,
                    cost_json,
                    payload_canon,
                    event.seq as i64,
                ],
            )
            .map_err(|e| GraphError::Log(format!("insert node: {e}")))?;

        // Materialize the structural PARENT_CHILD edges from this node to each
        // parent, so the projection's parent relation is complete from the
        // create event alone (DESIGN §6.3). Order is preserved via `seq`.
        for parent in &p.parent_ids {
            self.insert_edge(p.node_id, *parent, EdgeType::ParentChild, event.seq)?;
        }
        Ok(())
    }

    fn apply_edge_added(&mut self, event: &Event) -> Result<()> {
        let p: EdgeAddedPayload = Self::decode(event)?;
        self.insert_edge(p.from, p.to, p.edge_type, event.seq)
    }

    fn insert_edge(&self, from: Ulid, to: Ulid, edge_type: EdgeType, seq: u64) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO edge (from_id, to_id, edge_type, seq) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    from.to_string(),
                    to.to_string(),
                    edge_type.as_tag(),
                    seq as i64,
                ],
            )
            .map_err(|e| GraphError::Log(format!("insert edge: {e}")))?;
        Ok(())
    }

    fn apply_ref_created(&mut self, event: &Event) -> Result<()> {
        let p: RefCreatedPayload = Self::decode(event)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO ref (name, kind, target) VALUES (?1, ?2, ?3)",
                rusqlite::params![p.name, p.kind.as_tag(), p.to.to_string()],
            )
            .map_err(|e| GraphError::Log(format!("insert ref: {e}")))?;
        Ok(())
    }

    fn apply_ref_moved(&mut self, event: &Event) -> Result<()> {
        let p: RefMovedPayload = Self::decode(event)?;
        // A move updates only the target; a move of an unknown ref is a no-op so
        // the fold never fails on an out-of-order or pruned ref.
        self.conn
            .execute(
                "UPDATE ref SET target = ?2 WHERE name = ?1",
                rusqlite::params![p.name, p.to.to_string()],
            )
            .map_err(|e| GraphError::Log(format!("move ref: {e}")))?;
        Ok(())
    }

    fn apply_node_status_changed(&mut self, event: &Event) -> Result<()> {
        let p: NodeStatusChangedPayload = Self::decode(event)?;
        // Record `stale_since` as the event-derived timestamp (the ULID's
        // millisecond component) so the value is deterministic on replay rather
        // than reading the wall clock.
        let stale_since: Option<i64> = if p.is_stale {
            Some(event.event_id.timestamp_ms() as i64)
        } else {
            None
        };
        self.conn
            .execute(
                "UPDATE node SET status = ?2, is_stale = ?3, stale_since = ?4, stale_reason = ?5 \
                 WHERE id = ?1",
                rusqlite::params![
                    p.node_id.to_string(),
                    lifecycle_tag(p.status),
                    p.is_stale as i64,
                    stale_since,
                    if p.is_stale { p.stale_reason } else { None },
                ],
            )
            .map_err(|e| GraphError::Log(format!("update node status: {e}")))?;
        Ok(())
    }

    /// Whether a node with `id` exists in the projection.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn node_exists(&self, id: Ulid) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM node WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(|e| GraphError::Log(format!("node_exists: {e}")))?;
        Ok(n > 0)
    }

    /// Read back a node's full [`NodeEnvelope`], or `None` if it does not exist.
    ///
    /// The returned envelope materializes `child_ids` from the inverse parent
    /// relation (`edge` rows of type `PARENT_CHILD` whose `to` is this node).
    /// The stored payload bytes are **not** returned here — the envelope is the
    /// payload-independent contract; payload access (with lazy upgrade) is a
    /// service-layer concern.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query/decode failure.
    pub fn get_node(&self, id: Ulid) -> Result<Option<NodeEnvelope>> {
        let row = self.conn.query_row(
            "SELECT kind, family, owns_snapshot, branch_id, status, is_stale, stale_since, \
             stale_reason, snapshot_hash, lineage_hash, payload_schema_version, op_log_id, \
             model, cost FROM node WHERE id = ?1",
            [id.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,          // kind
                    r.get::<_, String>(1)?,          // family
                    r.get::<_, i64>(2)?,             // owns_snapshot
                    r.get::<_, String>(3)?,          // branch_id
                    r.get::<_, String>(4)?,          // status
                    r.get::<_, i64>(5)?,             // is_stale
                    r.get::<_, Option<i64>>(6)?,     // stale_since
                    r.get::<_, Option<String>>(7)?,  // stale_reason
                    r.get::<_, Option<String>>(8)?,  // snapshot_hash
                    r.get::<_, String>(9)?,          // lineage_hash
                    r.get::<_, i64>(10)?,            // payload_schema_version
                    r.get::<_, String>(11)?,         // op_log_id
                    r.get::<_, Option<String>>(12)?, // model
                    r.get::<_, Option<String>>(13)?, // cost
                ))
            },
        );
        let row = match row {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(GraphError::Log(format!("get_node: {e}"))),
        };

        let parent_ids = self.parents_of_result(id)?;
        let child_ids = self.children_of(id)?;

        let cost = match &row.13 {
            Some(s) => Some(parse_cost(s)?),
            None => None,
        };

        Ok(Some(NodeEnvelope {
            id,
            kind: row.0,
            family: parse_family(&row.1)?,
            owns_snapshot: row.2 != 0,
            parent_ids,
            child_ids,
            branch_id: row.3,
            status: parse_lifecycle(&row.4)?,
            is_stale: row.5 != 0,
            stale_since: row.6.map(|v| v as u64),
            stale_reason: row.7,
            snapshot_hash: parse_opt_hash(row.8.as_deref())?,
            model: row.12,
            cost,
            lineage_hash: parse_hash(&row.9)?,
            payload_schema_version: u16::try_from(row.10).map_err(|_| {
                GraphError::Log(format!("payload_schema_version {} out of range", row.10))
            })?,
            op_log_id: parse_ulid(&row.11)?,
        }))
    }

    /// Read the canonical stored payload bytes for a node, if it exists.
    ///
    /// This is the raw, un-upgraded payload (the bytes folded from the
    /// `graph.node_created` event). The service layer applies lazy
    /// upgrade-on-read on top of these bytes without rewriting the row
    /// (CLAUDE.md C5, DESIGN §7.2).
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn raw_payload(&self, id: Ulid) -> Result<Option<(serde_json::Value, u16)>> {
        let res = self.conn.query_row(
            "SELECT payload, payload_schema_version FROM node WHERE id = ?1",
            [id.to_string()],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
        );
        match res {
            Ok((bytes, ver)) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|e| GraphError::Log(format!("payload not valid JSON: {e}")))?;
                let ver = u16::try_from(ver).map_err(|_| {
                    GraphError::Log(format!("payload_schema_version {ver} out of range"))
                })?;
                Ok(Some((value, ver)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(GraphError::Log(format!("raw_payload: {e}"))),
        }
    }

    /// The direct parents of `node` via `PARENT_CHILD` and `MERGE_PARENT` edges,
    /// in stable `(seq, to)` order — the materialized `parent_ids`.
    fn parents_of_result(&self, node: Ulid) -> Result<Vec<Ulid>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT to_id FROM edge WHERE from_id = ?1 AND edge_type IN ('PARENT_CHILD', 'MERGE_PARENT') \
                 ORDER BY seq ASC, to_id ASC",
            )
            .map_err(|e| GraphError::Log(format!("prepare parents: {e}")))?;
        let rows = stmt
            .query_map([node.to_string()], |r| r.get::<_, String>(0))
            .map_err(|e| GraphError::Log(format!("query parents: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| GraphError::Log(format!("row parents: {e}")))?;
            out.push(parse_ulid(&s)?);
        }
        Ok(out)
    }

    /// The direct children of `node`: nodes that have `node` as a parent via a
    /// `PARENT_CHILD` *or* `MERGE_PARENT` edge — the exact inverse of
    /// [`parents_of_result`](GraphProjection::parents_of_result), so `child_ids`
    /// and `parent_ids` are mutual inverses (a merge node appears in each of its
    /// merge-parents' `child_ids`). Keeping the two relations symmetric is what
    /// lets the staleness walk down `child_ids` reach every descendant of a
    /// changed mutating node, including a merge node reached through a
    /// merge-parent (DESIGN §6.3, §7.3).
    fn children_of(&self, node: Ulid) -> Result<Vec<Ulid>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT from_id FROM edge WHERE to_id = ?1 AND edge_type IN ('PARENT_CHILD', 'MERGE_PARENT') \
                 ORDER BY seq ASC, from_id ASC",
            )
            .map_err(|e| GraphError::Log(format!("prepare children: {e}")))?;
        let rows = stmt
            .query_map([node.to_string()], |r| r.get::<_, String>(0))
            .map_err(|e| GraphError::Log(format!("query children: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| GraphError::Log(format!("row children: {e}")))?;
            out.push(parse_ulid(&s)?);
        }
        Ok(out)
    }

    /// Materialize the entire projection as a canonical, comparable
    /// [`ProjectionState`].
    ///
    /// Every node, edge, and ref is read into ordered containers, so the result
    /// is independent of SQLite row order. Used by
    /// [`canonical_digest`](GraphProjection::canonical_digest) and by tests that
    /// assert drop-and-rebuild identity.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query/decode failure.
    pub fn state(&self) -> Result<ProjectionState> {
        let mut nodes = BTreeMap::new();
        let ids = self.all_node_ids()?;
        for id in ids {
            if let Some(env) = self.get_node(id)? {
                nodes.insert(id.to_string(), env);
            }
        }

        let mut edges = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT from_id, to_id, edge_type FROM edge")
                .map_err(|e| GraphError::Log(format!("prepare edges: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| GraphError::Log(format!("query edges: {e}")))?;
            for r in rows {
                let (f, t, et) = r.map_err(|e| GraphError::Log(format!("row edge: {e}")))?;
                edges.push(EdgeRow {
                    from: parse_ulid(&f)?,
                    to: parse_ulid(&t)?,
                    edge_type: parse_edge_type(&et)?,
                });
            }
        }
        // Sort so the canonical bytes are independent of row order.
        edges.sort();

        let mut refs = BTreeMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT name, kind, target FROM ref")
                .map_err(|e| GraphError::Log(format!("prepare refs: {e}")))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| GraphError::Log(format!("query refs: {e}")))?;
            for r in rows {
                let (name, kind, target) =
                    r.map_err(|e| GraphError::Log(format!("row ref: {e}")))?;
                refs.insert(
                    name,
                    RefRow {
                        kind: parse_ref_kind(&kind)?,
                        target: parse_ulid(&target)?,
                    },
                );
            }
        }

        Ok(ProjectionState { nodes, edges, refs })
    }

    /// A BLAKE3 digest over the canonical bytes of the whole projection state.
    ///
    /// This is the "projection == log" witness: two projections folded from the
    /// same event sequence produce an identical digest, so dropping and
    /// rebuilding the projection is provably non-lossy (DESIGN §6.1, A.6).
    ///
    /// # Errors
    ///
    /// - [`GraphError::Log`] on a query/decode failure.
    /// - [`GraphError::Canon`] if the state cannot be canonicalized.
    pub fn canonical_digest(&self) -> Result<Hash> {
        let state = self.state()?;
        let bytes = spork_canon::canonicalize(&state)?;
        Ok(spork_hash::hash_bytes(&bytes))
    }

    /// All node ids, in ascending id order.
    fn all_node_ids(&self) -> Result<Vec<Ulid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM node ORDER BY id ASC")
            .map_err(|e| GraphError::Log(format!("prepare node ids: {e}")))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| GraphError::Log(format!("query node ids: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let s = r.map_err(|e| GraphError::Log(format!("row node id: {e}")))?;
            out.push(parse_ulid(&s)?);
        }
        Ok(out)
    }

    /// The number of nodes in the projection.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn node_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM node", [], |r| r.get(0))
            .map_err(|e| GraphError::Log(format!("node_count: {e}")))?;
        Ok(n as u64)
    }

    /// The number of edges in the projection.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn edge_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edge", [], |r| r.get(0))
            .map_err(|e| GraphError::Log(format!("edge_count: {e}")))?;
        Ok(n as u64)
    }

    /// The target a ref currently points at, or `None` if the ref is absent.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn ref_target(&self, name: &str) -> Result<Option<Ulid>> {
        let res = self
            .conn
            .query_row("SELECT target FROM ref WHERE name = ?1", [name], |r| {
                r.get::<_, String>(0)
            });
        match res {
            Ok(s) => Ok(Some(parse_ulid(&s)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(GraphError::Log(format!("ref_target: {e}"))),
        }
    }
}

/// The projection is the acyclicity oracle: [`Adjacency::parents_of`] reads the
/// materialized parent relation so the command layer can call
/// [`would_create_cycle`](spork_edges::would_create_cycle) against the *current*
/// graph before appending a new edge (DESIGN §6.3).
///
/// A query failure is surfaced as an empty parent list rather than a panic; the
/// command layer separately validates node existence, so a missing node never
/// silently admits a cycle. The fallible internal path
/// ([`parents_of_result`](GraphProjection::parents_of_result)) is used directly
/// by the materializer where errors must propagate.
impl Adjacency for GraphProjection {
    fn parents_of(&self, node: Ulid) -> Vec<Ulid> {
        self.parents_of_result(node).unwrap_or_default()
    }
}

// ---- column <-> enum marshaling (stable string tags) -----------------------

fn family_tag(f: Family) -> &'static str {
    f.as_tag()
}

fn parse_family(tag: &str) -> Result<Family> {
    Family::ALL
        .into_iter()
        .find(|f| f.as_tag() == tag)
        .ok_or_else(|| GraphError::Log(format!("unknown family tag {tag:?}")))
}

fn lifecycle_tag(l: Lifecycle) -> &'static str {
    match l {
        Lifecycle::Pending => "pending",
        Lifecycle::Running => "running",
        Lifecycle::Passed => "passed",
        Lifecycle::Failed => "failed",
        Lifecycle::Blocked => "blocked",
        Lifecycle::Cancelled => "cancelled",
    }
}

fn parse_lifecycle(tag: &str) -> Result<Lifecycle> {
    Ok(match tag {
        "pending" => Lifecycle::Pending,
        "running" => Lifecycle::Running,
        "passed" => Lifecycle::Passed,
        "failed" => Lifecycle::Failed,
        "blocked" => Lifecycle::Blocked,
        "cancelled" => Lifecycle::Cancelled,
        other => return Err(GraphError::Log(format!("unknown lifecycle tag {other:?}"))),
    })
}

fn parse_edge_type(tag: &str) -> Result<EdgeType> {
    EdgeType::ALL
        .into_iter()
        .find(|e| e.as_tag() == tag)
        .ok_or_else(|| GraphError::Log(format!("unknown edge_type tag {tag:?}")))
}

fn parse_ref_kind(tag: &str) -> Result<spork_edges::RefKind> {
    spork_edges::RefKind::ALL
        .into_iter()
        .find(|k| k.as_tag() == tag)
        .ok_or_else(|| GraphError::Log(format!("unknown ref kind tag {tag:?}")))
}

fn parse_ulid(s: &str) -> Result<Ulid> {
    Ulid::from_string(s).map_err(|e| GraphError::Log(format!("invalid ULID {s:?}: {e}")))
}

fn parse_hash(s: &str) -> Result<Hash> {
    Hash::from_hex(s).map_err(|e| GraphError::Log(format!("invalid hash {s:?}: {e}")))
}

fn parse_opt_hash(s: Option<&str>) -> Result<Option<Hash>> {
    match s {
        Some(s) => Ok(Some(parse_hash(s)?)),
        None => Ok(None),
    }
}

fn parse_cost(s: &str) -> Result<CostRecord> {
    serde_json::from_str(s).map_err(|e| GraphError::Log(format!("invalid cost record {s:?}: {e}")))
}

/// Extract a node's optional `cost` payload field as the JSON the `cost` column
/// stores (read back by [`parse_cost`]).
///
/// An agent-run node records the priced [`CostRecord`] of its model turn inside
/// its type-specific payload. Cost is derived **purely in this projection fold**
/// from the payload (no `cost` field on the `graph.node_created` event, no
/// event-bytes change) — intentionally *more* pure-projection than the `model`
/// column, which the service layer (`spork_graph::service::extract_model`)
/// persists onto `NodeCreatedPayload.model` and the fold then copies verbatim.
/// This validates the shape by round-tripping through [`CostRecord`], so only a
/// well-formed record is stored; an absent or malformed `cost` yields `None`,
/// leaving the column null exactly as in F2.
fn extract_cost_json(payload: &serde_json::Value) -> Option<String> {
    let value = payload.get("cost")?;
    let cost: CostRecord = serde_json::from_value(value.clone()).ok()?;
    serde_json::to_string(&cost).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spork_log::{Event, GENESIS_PREV_HASH};

    /// Build a synthetic [`Event`] with a chosen type/payload/seq for folding.
    fn event(seq: u64, event_type: &str, payload: serde_json::Value) -> Event {
        Event {
            event_id: Ulid::new(),
            seq,
            event_type: event_type.to_string(),
            schema_version: 1,
            payload,
            prev_event_hash: GENESIS_PREV_HASH,
            this_event_hash: GENESIS_PREV_HASH,
            actor: "test".into(),
        }
    }

    #[test]
    fn lifecycle_tags_round_trip_for_every_variant() {
        for l in Lifecycle::ALL {
            let tag = lifecycle_tag(l);
            assert_eq!(parse_lifecycle(tag).unwrap(), l);
        }
        assert!(parse_lifecycle("bogus").is_err());
    }

    #[test]
    fn edge_and_ref_tags_round_trip() {
        for e in EdgeType::ALL {
            assert_eq!(parse_edge_type(e.as_tag()).unwrap(), e);
        }
        assert!(parse_edge_type("BOGUS").is_err());
        for k in spork_edges::RefKind::ALL {
            assert_eq!(parse_ref_kind(k.as_tag()).unwrap(), k);
        }
        assert!(parse_ref_kind("Bogus").is_err());
    }

    #[test]
    fn family_tags_round_trip() {
        for f in Family::ALL {
            assert_eq!(parse_family(family_tag(f)).unwrap(), f);
        }
        assert!(parse_family("bogus").is_err());
    }

    #[test]
    fn fold_materializes_the_event_family_verbatim() {
        // A custom observing kind the projection could never classify from its
        // kind string still materializes as Observing because the event carries
        // the authoritative family (D-5).
        let mut proj = GraphProjection::open_in_memory().unwrap();
        let id = Ulid::new();
        proj.apply(&event(
            1,
            EVENT_NODE_CREATED,
            json!({
                "node_id": id.to_string(),
                "kind": "a11y_audit",
                "family": "observing",
                "type_version": "1.0.0",
                "parent_ids": [],
                "branch_id": "main",
                "owns_snapshot": false,
                "payload": {},
                "payload_schema_version": 1,
                "lineage_hash": spork_hash::hash_bytes(b"l").to_hex(),
            }),
        ))
        .unwrap();
        assert_eq!(
            proj.get_node(id).unwrap().unwrap().family,
            Family::Observing
        );
    }

    #[test]
    fn status_change_fold_updates_hot_columns_and_clears_on_unstale() {
        let mut proj = GraphProjection::open_in_memory().unwrap();
        let id = Ulid::new();
        let lineage = spork_hash::hash_bytes(b"l");
        // Create an observing node (starts pending).
        proj.apply(&event(
            1,
            EVENT_NODE_CREATED,
            json!({
                "node_id": id.to_string(),
                "kind": "validation",
                "family": "observing",
                "type_version": "1.0.0",
                "parent_ids": [],
                "branch_id": "main",
                "owns_snapshot": false,
                "payload": {},
                "payload_schema_version": 1,
                "lineage_hash": lineage.to_hex(),
            }),
        ))
        .unwrap();
        assert_eq!(
            proj.get_node(id).unwrap().unwrap().status,
            Lifecycle::Pending
        );

        // Mark it failed + stale.
        proj.apply(&event(
            2,
            EVENT_NODE_STATUS_CHANGED,
            json!({
                "node_id": id.to_string(),
                "status": "failed",
                "is_stale": true,
                "stale_reason": "parent changed",
            }),
        ))
        .unwrap();
        let env = proj.get_node(id).unwrap().unwrap();
        assert_eq!(env.status, Lifecycle::Failed);
        assert!(env.is_stale);
        assert_eq!(env.stale_reason.as_deref(), Some("parent changed"));
        assert!(env.stale_since.is_some());

        // Re-run: passed + not stale clears the staleness fields.
        proj.apply(&event(
            3,
            EVENT_NODE_STATUS_CHANGED,
            json!({
                "node_id": id.to_string(),
                "status": "passed",
                "is_stale": false,
            }),
        ))
        .unwrap();
        let env = proj.get_node(id).unwrap().unwrap();
        assert_eq!(env.status, Lifecycle::Passed);
        assert!(!env.is_stale);
        assert!(env.stale_reason.is_none());
        assert!(env.stale_since.is_none());
    }

    #[test]
    fn non_graph_events_are_ignored_by_the_fold() {
        let mut proj = GraphProjection::open_in_memory().unwrap();
        proj.apply(&event(1, "some.other.event", json!({ "x": 1 })))
            .unwrap();
        assert_eq!(proj.node_count().unwrap(), 0);
    }

    #[test]
    fn refold_is_idempotent() {
        // Folding the same node-created event twice yields one node (INSERT OR
        // REPLACE) and one parent edge (the edge primary key), so a checkpoint
        // replay and a from-scratch rebuild stay identical.
        let mut proj = GraphProjection::open_in_memory().unwrap();
        let parent = Ulid::new();
        let child = Ulid::new();
        let mk = |id: Ulid, parents: Vec<Ulid>, seq: u64| {
            event(
                seq,
                EVENT_NODE_CREATED,
                json!({
                    "node_id": id.to_string(),
                    "kind": "snapshot",
                    "family": "mutating",
                    "type_version": "1.0.0",
                    "parent_ids": parents.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                    "branch_id": "main",
                    "owns_snapshot": true,
                    "snapshot_hash": spork_hash::hash_bytes(id.to_string().as_bytes()).to_hex(),
                    "payload": { "origin": "manual" },
                    "payload_schema_version": 1,
                    "lineage_hash": spork_hash::hash_bytes(b"l").to_hex(),
                }),
            )
        };
        let ev_parent = mk(parent, vec![], 1);
        let ev_child = mk(child, vec![parent], 2);
        proj.apply(&ev_parent).unwrap();
        proj.apply(&ev_child).unwrap();
        let d1 = proj.canonical_digest().unwrap();
        // Re-apply both — idempotent.
        proj.apply(&ev_parent).unwrap();
        proj.apply(&ev_child).unwrap();
        let d2 = proj.canonical_digest().unwrap();
        assert_eq!(d1, d2);
        assert_eq!(proj.node_count().unwrap(), 2);
        assert_eq!(proj.edge_count().unwrap(), 1);
    }
}
