//! Drift attribution — the A.3 precedence algorithm (DESIGN.md §10.2, A.3).
//!
//! Mis-attribution is the trust-critical risk (a human edit blamed on the agent
//! is the §18 mis-attribution risk), so the resolution order from A.3 is made
//! explicit and testable here. Given the fused [`ChangeEvent`]s for one path,
//! the [`Attributor`] decides *who* changed it and how confident it is, applying
//! the exact A.3 decision tree:
//!
//! ```text
//! Q1 active agent turn wrote P this turn (interceptor saw it)?  -> Agent       (high)
//! Q2 editor buffer for P dirty + focused?                       -> HumanEditor (high)
//! Q3 within debounce window of a known tool/bash exec?          -> AgentBash   (medium)
//! Q4 any agent turn active at event time?                       -> AgentTentative (low, review)
//! else                                                          -> External    (medium)
//! ```
//!
//! **Precedence** (A.3): in-process interceptor evidence outranks fs-watcher
//! evidence for the same `(path, turn)` — the interceptor is authoritative on
//! *who*, the watcher only on *whether* something changed. Every low-confidence
//! attribution sets a [`review_flag`](AttributionRecord::review_flag) rather than
//! silently committing. The reconciliation rescan only ever *adds* drift records
//! ([`Attributor::ingest_rescan`]); it never rewrites an existing high-confidence
//! record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::source::{ChangeEvent, ChangeOp, ChangeSourceKind};

/// The current schema version of an [`AttributionRecord`] (CLAUDE.md C5).
pub const ATTRIBUTION_SCHEMA_VERSION: u16 = 1;

/// Who a change is attributed to (the A.3 attribution outcomes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attribution {
    /// An active agent turn wrote the path and the interceptor saw it (A.3 Q1).
    Agent,
    /// A change within the debounce window of a known agent tool/bash exec
    /// (A.3 Q3).
    AgentBash,
    /// A dirty, focused editor buffer — a human edit (A.3 Q2).
    HumanEditor,
    /// No agent turn was active and no interceptor/buffer evidence: an external
    /// actor (a build script, an unattended `mv`) (A.3 else-branch).
    External,
    /// An agent turn was active but no interceptor evidence pinned the write to
    /// it: attributed to the agent *tentatively*, flagged for review (A.3 Q4).
    AgentTentative,
}

impl Attribution {
    /// A short human label for diagnostics and the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Attribution::Agent => "agent",
            Attribution::AgentBash => "agent-bash",
            Attribution::HumanEditor => "human-editor",
            Attribution::External => "external",
            Attribution::AgentTentative => "agent-tentative",
        }
    }
}

/// The confidence of an [`Attribution`] (A.3).
///
/// Confidence drives the review flag: only [`Low`](Confidence::Low) confidence
/// sets [`AttributionRecord::review_flag`], the honest-fidelity stance §10.2
/// commits to (DESIGN.md A.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Confidence {
    /// Low confidence — sets the UI review flag (A.3 Q4).
    Low,
    /// Medium confidence — agent-bash heuristic or external (A.3 Q3/else).
    Medium,
    /// High confidence — interceptor or focused-buffer evidence (A.3 Q1/Q2).
    High,
}

impl Confidence {
    /// A short human label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

/// The per-path attribution outcome — user-correctable, schema-versioned (A.3).
///
/// This is the durable record the UI surfaces and the user can correct
/// ([`user_correctable`](AttributionRecord::user_correctable) is always `true`
/// in v1; it freezes the slot for a future policy that could pin a record). It
/// carries an explicit `schema_version` so it can gain fields without
/// invalidating older persisted drift metadata (CLAUDE.md C5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionRecord {
    /// Schema version of this record (CLAUDE.md C5).
    pub schema_version: u16,
    /// The repo-relative path the record is about (`/`-separated canonical form).
    pub path: String,
    /// What kind of mutation the path underwent.
    pub op: ChangeOp,
    /// Who the change is attributed to.
    pub attribution: Attribution,
    /// How confident the attribution is.
    pub confidence: Confidence,
    /// Set iff the attribution is low-confidence; the UI surfaces it for review
    /// rather than silently committing (DESIGN.md A.3).
    pub review_flag: bool,
    /// Whether the user may correct this record (always `true` in v1).
    pub user_correctable: bool,
}

impl AttributionRecord {
    /// Build a record from a path, op, attribution, and confidence, deriving the
    /// review flag from confidence (low → flagged) per A.3.
    fn new(path: &Path, op: ChangeOp, attribution: Attribution, confidence: Confidence) -> Self {
        AttributionRecord {
            schema_version: ATTRIBUTION_SCHEMA_VERSION,
            path: path_to_canonical(path),
            op,
            attribution,
            confidence,
            review_flag: confidence == Confidence::Low,
            user_correctable: true,
        }
    }
}

/// Render a path as the canonical `/`-separated repo-relative string.
fn path_to_canonical(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// The A.3 attribution engine: fuses per-path evidence into an
/// [`AttributionRecord`], honoring interceptor-over-watcher precedence and the
/// rescan-only-adds rule (DESIGN.md A.3).
///
/// The attributor is fed the [`ChangeEvent`]s for a debounce window plus a small
/// amount of side context (which turn is active, which buffers are focused-dirty,
/// when the last agent tool/bash exec happened). It groups events by path,
/// applies the A.3 decision tree per path with interceptor evidence taking
/// precedence, and emits one record per path. Rescan-only divergences are
/// admitted through [`ingest_rescan`](Attributor::ingest_rescan), which *adds*
/// records but never overwrites a high-confidence one already produced.
#[derive(Debug)]
pub struct Attributor {
    /// The debounce window for the A.3 Q3 "within debounce window of a known
    /// tool/bash exec" heuristic.
    debounce: Duration,
    /// The records produced so far, keyed by canonical path. Holding them lets
    /// `ingest_rescan` honor the "never rewrite a high-confidence record" rule.
    records: BTreeMap<String, AttributionRecord>,
}

/// Side context the attributor needs beyond the raw events (A.3 inputs).
///
/// This is the "active-focus correlation" §10.2 promises, made concrete: which
/// agent turn is active, when the agent last ran a tool/bash exec, and which
/// paths have a focused-dirty editor buffer.
#[derive(Debug, Default, Clone)]
pub struct TurnContext {
    /// The active agent turn id, if a turn is open (A.3 Q4).
    pub active_turn: Option<u64>,
    /// When the agent last spawned a tool/bash exec (A.3 Q3 debounce anchor).
    pub last_agent_exec: Option<Instant>,
    /// Repo-relative paths with a focused-dirty editor buffer (A.3 Q2).
    pub focused_dirty: Vec<PathBuf>,
}

impl TurnContext {
    /// Whether `path` has a focused-dirty buffer (A.3 Q2).
    fn is_focused_dirty(&self, path: &Path) -> bool {
        self.focused_dirty.iter().any(|p| p == path)
    }
}

impl Attributor {
    /// Create an attributor with the given Q3 debounce window.
    #[must_use]
    pub fn new(debounce: Duration) -> Self {
        Attributor {
            debounce,
            records: BTreeMap::new(),
        }
    }

    /// Borrow the records produced so far (read-only).
    #[must_use]
    pub fn records(&self) -> Vec<AttributionRecord> {
        self.records.values().cloned().collect()
    }

    /// Attribute a batch of fused change events under `ctx`, returning the
    /// records produced *for this batch* (also retained for the rescan rule).
    ///
    /// Events are grouped by path. For each path, the strongest evidence wins by
    /// A.3 precedence: an [`Interceptor`](ChangeSourceKind::Interceptor) event
    /// for the active turn yields a high-confidence [`Agent`](Attribution::Agent)
    /// record regardless of any concurrent fs-watcher event for the same path.
    /// The op recorded is taken from the highest-precedence event for the path.
    pub fn attribute(
        &mut self,
        events: &[ChangeEvent],
        ctx: &TurnContext,
    ) -> Vec<AttributionRecord> {
        // Group events per path, preserving the highest-precedence source seen.
        let mut by_path: BTreeMap<PathBuf, Vec<&ChangeEvent>> = BTreeMap::new();
        for ev in events {
            by_path.entry(ev.path.clone()).or_default().push(ev);
        }

        let mut produced = Vec::new();
        for (path, evs) in by_path {
            let record = self.attribute_path(&path, &evs, ctx);
            // Precedence on merge: a higher-confidence new record supersedes a
            // weaker prior one for the same path; an equal-or-lower one defers to
            // the existing high-confidence record (mirrors the rescan rule).
            self.merge_record(record.clone());
            produced.push(record);
        }
        produced
    }

    /// Apply the A.3 decision tree for one path given its events and context.
    fn attribute_path(
        &self,
        path: &Path,
        events: &[&ChangeEvent],
        ctx: &TurnContext,
    ) -> AttributionRecord {
        // The op is the highest-precedence event's op (interceptor first, then
        // buffer, then watcher, then rescan); falls back to the first event.
        let op = pick_op(events);

        // --- A.3 Q1: did the interceptor see the active turn write this path? ---
        // Precedence: interceptor evidence for the active turn is authoritative
        // on *who*, outranking any concurrent fs-watcher event for the same path.
        let interceptor_this_turn = events.iter().any(|e| {
            e.source == ChangeSourceKind::Interceptor
                && e.turn.is_some()
                && e.turn == ctx.active_turn
        });
        if interceptor_this_turn {
            return AttributionRecord::new(path, op, Attribution::Agent, Confidence::High);
        }
        // An interceptor event with *some* turn (even if the context turn is not
        // tracked) is still authoritative-on-who agent evidence.
        let interceptor_any_turn = events
            .iter()
            .any(|e| e.source == ChangeSourceKind::Interceptor && e.turn.is_some());
        if interceptor_any_turn {
            return AttributionRecord::new(path, op, Attribution::Agent, Confidence::High);
        }

        // --- A.3 Q2: dirty + focused editor buffer => human edit. ---
        let buffer_here = events
            .iter()
            .any(|e| e.source == ChangeSourceKind::BufferBridge);
        if buffer_here || ctx.is_focused_dirty(path) {
            // A focused-dirty buffer is high-confidence human evidence; a
            // non-focused buffer event still counts as a human edit but the
            // focus signal is what makes it high-confidence (A.3 Q2).
            let confidence = if ctx.is_focused_dirty(path) {
                Confidence::High
            } else {
                Confidence::Medium
            };
            return AttributionRecord::new(path, op, Attribution::HumanEditor, confidence);
        }

        // --- A.3 Q3: within the debounce window of a known tool/bash exec? ---
        let event_time = events
            .iter()
            .map(|e| e.observed_at)
            .min()
            .unwrap_or_else(Instant::now);
        if let Some(exec) = ctx.last_agent_exec {
            // The event must be at or after the exec and within the window.
            if event_time >= exec && event_time.duration_since(exec) <= self.debounce {
                return AttributionRecord::new(
                    path,
                    op,
                    Attribution::AgentBash,
                    Confidence::Medium,
                );
            }
        }

        // --- A.3 Q4: any agent turn active at event time? ---
        if ctx.active_turn.is_some() {
            // Tentative, low confidence, flagged for review (A.3 Q4).
            return AttributionRecord::new(path, op, Attribution::AgentTentative, Confidence::Low);
        }

        // --- A.3 else: external actor, medium confidence. ---
        AttributionRecord::new(path, op, Attribution::External, Confidence::Medium)
    }

    /// Admit rescan-only divergences: *add* a record for any path the rescan saw
    /// that has no record yet, but never rewrite an existing high-confidence one
    /// (DESIGN.md A.3 "the rescan only ever adds; never rewrites").
    ///
    /// Rescan events have no turn and no interceptor evidence, so a fresh path
    /// they reveal is attributed [`External`](Attribution::External) (no agent
    /// turn) or [`AgentTentative`](Attribution::AgentTentative) (a turn is open)
    /// per the same A.3 tree.
    pub fn ingest_rescan(
        &mut self,
        events: &[ChangeEvent],
        ctx: &TurnContext,
    ) -> Vec<AttributionRecord> {
        let mut added = Vec::new();
        let mut by_path: BTreeMap<PathBuf, Vec<&ChangeEvent>> = BTreeMap::new();
        for ev in events {
            debug_assert_eq!(ev.source, ChangeSourceKind::Rescan);
            by_path.entry(ev.path.clone()).or_default().push(ev);
        }
        for (path, evs) in by_path {
            let canonical = path_to_canonical(&path);
            // Never rewrite an existing high-confidence record (A.3).
            if let Some(existing) = self.records.get(&canonical) {
                if existing.confidence == Confidence::High {
                    continue;
                }
            }
            let record = self.attribute_path(&path, &evs, ctx);
            // The rescan only *adds*: if a record already exists (and is not
            // high-confidence), keep the existing one and skip — the rescan must
            // not silently rewrite attribution it cannot improve on.
            if self.records.contains_key(&canonical) {
                continue;
            }
            self.records.insert(canonical, record.clone());
            added.push(record);
        }
        added
    }

    /// Apply a freshly produced record, respecting precedence: a strictly
    /// higher-confidence record supersedes a weaker prior one for the same path;
    /// otherwise the existing record stands (the interceptor-over-watcher and
    /// rescan-only-adds invariants both reduce to this).
    fn merge_record(&mut self, record: AttributionRecord) {
        match self.records.get(&record.path) {
            Some(existing) if existing.confidence >= record.confidence => {}
            _ => {
                self.records.insert(record.path.clone(), record);
            }
        }
    }
}

/// Pick the op for a path from its events by source precedence
/// (interceptor > buffer > watcher > rescan), falling back to the first event.
fn pick_op(events: &[&ChangeEvent]) -> ChangeOp {
    const ORDER: [ChangeSourceKind; 4] = [
        ChangeSourceKind::Interceptor,
        ChangeSourceKind::BufferBridge,
        ChangeSourceKind::FsWatcher,
        ChangeSourceKind::Rescan,
    ];
    for src in ORDER {
        if let Some(e) = events.iter().find(|e| e.source == src) {
            return e.op;
        }
    }
    events.first().map(|e| e.op).unwrap_or(ChangeOp::Modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ChangeSourceKind;
    use std::path::PathBuf;
    use std::time::Instant;

    fn ev(path: &str, op: ChangeOp, source: ChangeSourceKind, turn: Option<u64>) -> ChangeEvent {
        ChangeEvent {
            path: PathBuf::from(path),
            op,
            source,
            turn,
            observed_at: Instant::now(),
        }
    }

    fn only(records: &[AttributionRecord]) -> &AttributionRecord {
        assert_eq!(records.len(), 1, "{records:?}");
        &records[0]
    }

    #[test]
    fn q1_interceptor_for_active_turn_is_high_confidence_agent() {
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext {
            active_turn: Some(9),
            ..Default::default()
        };
        let events = [ev(
            "src/a.rs",
            ChangeOp::Modified,
            ChangeSourceKind::Interceptor,
            Some(9),
        )];
        let rec = only(&a.attribute(&events, &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::Agent);
        assert_eq!(rec.confidence, Confidence::High);
        assert!(!rec.review_flag);
        assert_eq!(rec.path, "src/a.rs");
        assert_eq!(rec.schema_version, ATTRIBUTION_SCHEMA_VERSION);
    }

    #[test]
    fn interceptor_outranks_watcher_for_same_path_and_turn() {
        // A.3 PRECEDENCE: the interceptor is authoritative on *who*; a concurrent
        // fs-watcher event for the same path must not downgrade the attribution.
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext {
            active_turn: Some(3),
            ..Default::default()
        };
        let events = [
            ev(
                "src/x.rs",
                ChangeOp::Modified,
                ChangeSourceKind::FsWatcher,
                None,
            ),
            ev(
                "src/x.rs",
                ChangeOp::Modified,
                ChangeSourceKind::Interceptor,
                Some(3),
            ),
        ];
        let rec = only(&a.attribute(&events, &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::Agent);
        assert_eq!(rec.confidence, Confidence::High);
    }

    #[test]
    fn q2_focused_dirty_buffer_is_high_confidence_human() {
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext {
            active_turn: Some(1),
            focused_dirty: vec![PathBuf::from("src/h.rs")],
            ..Default::default()
        };
        // Even with a turn active, a focused-dirty buffer wins as a human edit.
        let events = [ev(
            "src/h.rs",
            ChangeOp::Modified,
            ChangeSourceKind::FsWatcher,
            None,
        )];
        let rec = only(&a.attribute(&events, &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::HumanEditor);
        assert_eq!(rec.confidence, Confidence::High);
    }

    #[test]
    fn q3_within_debounce_window_of_exec_is_agent_bash() {
        let mut a = Attributor::new(Duration::from_secs(10));
        let exec = Instant::now();
        let ctx = TurnContext {
            active_turn: Some(2),
            last_agent_exec: Some(exec),
            ..Default::default()
        };
        // A watcher-only event right after a known agent exec, within the window.
        let mut event = ev(
            "out/gen.rs",
            ChangeOp::Added,
            ChangeSourceKind::FsWatcher,
            None,
        );
        event.observed_at = exec + Duration::from_millis(5);
        let rec = only(&a.attribute(&[event], &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::AgentBash);
        assert_eq!(rec.confidence, Confidence::Medium);
        assert!(!rec.review_flag);
    }

    #[test]
    fn q4_turn_active_but_no_evidence_is_low_confidence_tentative() {
        let mut a = Attributor::new(Duration::from_millis(1));
        let ctx = TurnContext {
            active_turn: Some(5),
            // No exec anchor, so Q3 cannot fire.
            ..Default::default()
        };
        let events = [ev(
            "x.rs",
            ChangeOp::Modified,
            ChangeSourceKind::FsWatcher,
            None,
        )];
        let rec = only(&a.attribute(&events, &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::AgentTentative);
        assert_eq!(rec.confidence, Confidence::Low);
        assert!(rec.review_flag, "low confidence must flag for review");
    }

    #[test]
    fn else_no_turn_is_external_medium() {
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext::default(); // no active turn
        let events = [ev(
            "script_out.txt",
            ChangeOp::Added,
            ChangeSourceKind::FsWatcher,
            None,
        )];
        let rec = only(&a.attribute(&events, &ctx)).clone();
        assert_eq!(rec.attribution, Attribution::External);
        assert_eq!(rec.confidence, Confidence::Medium);
        assert!(!rec.review_flag);
    }

    #[test]
    fn rescan_only_adds_never_rewrites_high_confidence() {
        // First, a high-confidence agent record for the path via the interceptor.
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext {
            active_turn: Some(7),
            ..Default::default()
        };
        a.attribute(
            &[ev(
                "src/a.rs",
                ChangeOp::Modified,
                ChangeSourceKind::Interceptor,
                Some(7),
            )],
            &ctx,
        );
        // A rescan later observes the same path. It must NOT rewrite the record.
        let added = a.ingest_rescan(
            &[ev(
                "src/a.rs",
                ChangeOp::Modified,
                ChangeSourceKind::Rescan,
                None,
            )],
            &ctx,
        );
        assert!(
            added.is_empty(),
            "rescan must not rewrite a high-confidence record"
        );
        let rec = a
            .records()
            .into_iter()
            .find(|r| r.path == "src/a.rs")
            .unwrap();
        assert_eq!(rec.attribution, Attribution::Agent);
        assert_eq!(rec.confidence, Confidence::High);
    }

    #[test]
    fn rescan_adds_a_record_for_a_path_with_none_yet() {
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext::default(); // no turn -> external
        let added = a.ingest_rescan(
            &[ev(
                "orphan.rs",
                ChangeOp::Added,
                ChangeSourceKind::Rescan,
                None,
            )],
            &ctx,
        );
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].attribution, Attribution::External);
        assert_eq!(a.records().len(), 1);
    }

    #[test]
    fn records_accumulate_across_paths() {
        let mut a = Attributor::new(Duration::from_millis(250));
        let ctx = TurnContext {
            active_turn: Some(1),
            ..Default::default()
        };
        a.attribute(
            &[
                ev(
                    "a.rs",
                    ChangeOp::Modified,
                    ChangeSourceKind::Interceptor,
                    Some(1),
                ),
                ev(
                    "b.rs",
                    ChangeOp::Added,
                    ChangeSourceKind::Interceptor,
                    Some(1),
                ),
            ],
            &ctx,
        );
        assert_eq!(a.records().len(), 2);
    }
}
