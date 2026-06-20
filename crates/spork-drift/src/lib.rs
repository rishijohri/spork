//! Spork F3 drift capture — closing the untracked-mutation gap.
//!
//! Not every change to a working tree flows through Spork's own edit path. This
//! crate captures the rest: it fuses three change sources — a precise in-process
//! [`EditInterceptor`], an OS [`FsWatcher`] (via `notify`), and a content-hash
//! [`ReconciliationRescan`] of the tree against the last snapshot, plus a
//! [`BufferBridge`] for unsaved buffers — and turns the fused stream into an
//! attributed, secret-safe drift node.
//!
//! Each change is run through the [`Attributor`], which applies the A.3
//! precedence rule: the interceptor outranks the fs-watcher for the same
//! `(path, turn)`, the rescan only *adds* drift records and never rewrites a
//! high-confidence one, and a low-confidence attribution is flagged for review.
//! Before anything is stored, the [`SecretScanner`] runs a regex set for common
//! secret shapes (AWS keys, generic `api_key=`, PEM private keys, tokens);
//! matched files are excluded so a secret never enters the CAS. The surviving
//! non-secret tree is put into [`spork_cas`] and recorded as a built-in
//! `snapshot` drift node (`origin = auto_drift`) through the F2
//! [`GraphService`](spork_graph::GraphService).
//!
//! This realizes the drift-capture model in DESIGN.md §10.1 ("Three-Layer
//! Architecture") and §10.2 ("Closing the Untracked-Mutation Gap"), the
//! attribution precedence in A.3 ("Drift Attribution Algorithm"), and the
//! secret-scan-at-capture requirement in §15.5 ("Privacy & Data Governance").
//!
//! # The pipeline
//!
//! 1. **Fuse** ([`source`]): every [`ChangeSource`] is polled for a batch of
//!    [`ChangeEvent`]s. Sources are the seam ([`ChangeSource`]); the four
//!    built-in impls are [`EditInterceptor`], [`FsWatcher`],
//!    [`ReconciliationRescan`], and [`BufferBridge`].
//! 2. **Attribute** ([`attribution`]): the [`Attributor`] applies the A.3
//!    decision tree to each path, with interceptor-over-watcher precedence and
//!    the rescan-only-adds rule, emitting a schema-versioned
//!    [`AttributionRecord`] per path.
//! 3. **Secret-scan & capture** ([`capture`]): [`DriftCapture`] secret-scans
//!    each file at capture, stages only non-secret files (applying unsaved
//!    buffers), puts that staging tree into the store, and records an
//!    `origin = auto_drift` node.
//!
//! # Worked example
//!
//! ```no_run
//! use std::time::Duration;
//! use spork_drift::{DriftCapture, EditInterceptor, ChangeOp, ChangeSource, TurnContext};
//!
//! let mut interceptor = EditInterceptor::new();
//! interceptor.begin_turn(7);
//! interceptor.record("src/main.rs", ChangeOp::Modified);
//!
//! let capture = DriftCapture::new("/path/to/worktree");
//! let ctx = TurnContext { active_turn: Some(7), ..Default::default() };
//! let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut interceptor];
//! let records = capture.fuse_and_attribute(&mut sources, &ctx);
//! assert_eq!(records.len(), 1);
//! // `capture.capture(store, graph, parents, branch, records, buffers)` then
//! // secret-scans, stores the non-secret tree, and records the auto-drift node.
//! ```
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! Every persisted struct carries an explicit `schema_version`:
//! [`AttributionRecord`] ([`ATTRIBUTION_SCHEMA_VERSION`]) and the
//! [`DriftCaptureReport`] ([`DRIFT_REPORT_SCHEMA_VERSION`]), and the secret
//! policy is versioned ([`SECRET_POLICY_VERSION`]). The drift node's snapshot
//! object is content-addressed and schema-versioned by [`spork_cas`].
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attribution;
mod capture;
mod error;
mod secret;
mod source;

pub use attribution::{
    Attribution, AttributionRecord, Attributor, Confidence, TurnContext, ATTRIBUTION_SCHEMA_VERSION,
};
pub use capture::{DriftCapture, DriftCaptureReport, DRIFT_REPORT_SCHEMA_VERSION};
pub use error::{DriftError, Result};
pub use secret::{SecretScanner, SECRET_POLICY_VERSION};
pub use source::{
    BufferBridge, ChangeEvent, ChangeOp, ChangeSource, ChangeSourceKind, EditInterceptor,
    FsWatcher, ReconciliationRescan,
};
