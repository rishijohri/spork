//! Spork lifecycle and effective-status tokens.
//!
//! Spork separates a node's *lifecycle* (where it is in its execution, e.g.
//! pending → running → passed/failed) from its *staleness* (whether its result
//! still reflects the current snapshot). These are two orthogonal axes; the UI
//! never renders the raw lifecycle directly. Instead it renders a single
//! *effective status* token derived by a total fold over `(Lifecycle, is_stale)`
//! — green / green-stale / red / red-stale / running / pending / blocked /
//! cancelled. Cancelled is itself a terminal, non-stale token (there is no
//! `cancelled_stale`), so every lifecycle state maps to exactly one UI token.
//!
//! This realizes the two-axis lifecycle model in DESIGN.md §7.3 ("Lifecycle: two
//! orthogonal axes") and feeds the node-envelope status surface in DESIGN.md
//! §6.2 ("Nodes: envelope plus payload").
//!
//! # The two axes
//!
//! Per DESIGN §7.3, staleness is modeled as an orthogonal boolean
//! (`is_stale`, plus `stale_since` / `stale_reason` recorded elsewhere on the
//! node envelope), **not** as a sixth lifecycle state. Folding staleness into
//! the [`Lifecycle`] set would double every outcome (`passed-stale`,
//! `failed-stale`, …) and discard the original run result. Keeping the axes
//! separate lets the engine cheaply invalidate a whole subtree by walking
//! `child_ids` on any mutating-node change *without* recomputing outcomes,
//! while the UI still renders "passed but stale" as one amber-over-green token.
//!
//! Consumers must never hand-check the two fields. The centralized
//! [`effective_status`] fold is the single, total mapping from
//! `(Lifecycle, is_stale)` to one [`EffectiveStatus`] UI token.
//!
//! # The cancelled token (the F0 / A.7 fix)
//!
//! `Cancelled` is a real UI token, not an alias for `pending` or `blocked`.
//! A cancelled run is terminal and is **not** subject to staleness: there is no
//! `cancelled_stale`, so [`effective_status`] maps `Cancelled` to
//! [`EffectiveStatus::Cancelled`] regardless of the `is_stale` flag.
//!
//! # Example
//!
//! ```
//! use spork_status::{effective_status, EffectiveStatus, Lifecycle};
//!
//! // A passed node whose parent snapshot has since changed renders amber.
//! assert_eq!(
//!     effective_status(Lifecycle::Passed, true),
//!     EffectiveStatus::GreenStale
//! );
//!
//! // Cancellation is terminal and never stale.
//! assert_eq!(
//!     effective_status(Lifecycle::Cancelled, true),
//!     EffectiveStatus::Cancelled
//! );
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version for the persisted status enums in this crate.
///
/// Per constraint C5, every persisted struct/enum carries a schema version with
/// a registered forward migration from its first commit. [`Lifecycle`] is the
/// `status` value persisted in graph events and the graph projection (DESIGN
/// §6.2, §7.3); this constant tags the wire form so a future generation can be
/// migrated additively without editing stored events.
pub const SCHEMA_VERSION: u16 = 1;

/// A node's lifecycle state — where it is in its execution.
///
/// This is the first of the two orthogonal axes described in DESIGN §7.3. The
/// state machine is `Pending → Running → {Passed | Failed | Blocked |
/// Cancelled}`. Staleness is the *second*, orthogonal axis and is **not**
/// represented here; see [`effective_status`] and the [`is_stale`] flag it
/// folds in.
///
/// `Lifecycle` is the persisted `status` field of a node envelope (DESIGN §6.2),
/// promoted into an indexed projection column rather than buried in payload
/// JSON. Its serialized form is `snake_case` so it is stable and human-legible
/// in stored events.
///
/// [`is_stale`]: effective_status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Created but not yet started (the entry state for observing nodes).
    Pending,
    /// Actively executing.
    Running,
    /// Terminal: the run completed successfully.
    Passed,
    /// Terminal: the run completed unsuccessfully.
    Failed,
    /// Waiting on an unmet dependency; cannot make progress.
    Blocked,
    /// Terminal: the run was cancelled. Terminal-and-non-stale — there is no
    /// `cancelled_stale` (DESIGN §7.3).
    Cancelled,
}

impl Lifecycle {
    /// Every [`Lifecycle`] variant, in declaration order.
    ///
    /// Useful for exhaustive iteration in tests and tooling. The length of this
    /// slice is part of the contract: adding a variant is a breaking change and
    /// must go through a versioned generation (constraint C2).
    pub const ALL: [Lifecycle; 6] = [
        Lifecycle::Pending,
        Lifecycle::Running,
        Lifecycle::Passed,
        Lifecycle::Failed,
        Lifecycle::Blocked,
        Lifecycle::Cancelled,
    ];

    /// Whether this lifecycle state is terminal (no further transition without
    /// a re-run that produces a new run id).
    ///
    /// Terminal states are `Passed`, `Failed`, and `Cancelled`. `Pending`,
    /// `Running`, and `Blocked` are non-terminal. Per DESIGN §7.3, only the
    /// non-`Cancelled` terminal outcomes (`Passed`, `Failed`) participate in
    /// staleness.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Lifecycle::Passed | Lifecycle::Failed | Lifecycle::Cancelled
        )
    }
}

/// A single UI status token: the result of folding `(Lifecycle, is_stale)`.
///
/// These are the *only* tokens the UI renders for a node's status (DESIGN §7.3).
/// `Green`/`Red` and their `*Stale` companions encode the `Passed`/`Failed`
/// outcomes crossed with the orthogonal staleness flag; `Running`, `Pending`,
/// and `Blocked` pass through directly; `Cancelled` is terminal and never
/// stale. Crucially `Cancelled` *is* a token here (the F0 / A.7 fix) — it is
/// not collapsed into another state.
///
/// The set is closed: there are exactly eight tokens and no `CancelledStale`.
/// Its serialized form is `snake_case`, matching the token names used by the UI
/// layer (`green`, `green_stale`, `red`, `red_stale`, `running`, `pending`,
/// `blocked`, `cancelled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStatus {
    /// Passed and current — rendered green.
    Green,
    /// Passed but stale (a parent snapshot changed since the run) — rendered
    /// amber over green.
    GreenStale,
    /// Failed and current — rendered red.
    Red,
    /// Failed but stale — rendered amber over red.
    RedStale,
    /// Actively executing.
    Running,
    /// Created but not yet started.
    Pending,
    /// Waiting on an unmet dependency.
    Blocked,
    /// Cancelled — terminal and non-stale, rendered grey/struck. There is no
    /// `cancelled_stale` (DESIGN §7.3).
    Cancelled,
}

impl EffectiveStatus {
    /// Every [`EffectiveStatus`] token, in declaration order.
    ///
    /// The length is part of the contract: exactly eight tokens, with no
    /// `CancelledStale`.
    pub const ALL: [EffectiveStatus; 8] = [
        EffectiveStatus::Green,
        EffectiveStatus::GreenStale,
        EffectiveStatus::Red,
        EffectiveStatus::RedStale,
        EffectiveStatus::Running,
        EffectiveStatus::Pending,
        EffectiveStatus::Blocked,
        EffectiveStatus::Cancelled,
    ];

    /// Whether this token reflects a stale result (`GreenStale` or `RedStale`).
    ///
    /// Only the two `*Stale` outcome tokens are stale; pass-through and terminal
    /// tokens (including `Cancelled`) are never stale.
    #[must_use]
    pub const fn is_stale(self) -> bool {
        matches!(
            self,
            EffectiveStatus::GreenStale | EffectiveStatus::RedStale
        )
    }
}

/// The total fold from a node's `(Lifecycle, is_stale)` pair to its single UI
/// [`EffectiveStatus`] token.
///
/// This is the *one* place the two lifecycle axes (DESIGN §7.3) are combined.
/// It is total: every `(Lifecycle, is_stale)` input yields exactly one token,
/// so callers never branch on the raw lifecycle or the staleness flag by hand.
///
/// # Mapping
///
/// | `status`    | `is_stale` | token         |
/// |-------------|------------|---------------|
/// | `Passed`    | `false`    | `Green`       |
/// | `Passed`    | `true`     | `GreenStale`  |
/// | `Failed`    | `false`    | `Red`         |
/// | `Failed`    | `true`     | `RedStale`    |
/// | `Running`   | any        | `Running`     |
/// | `Pending`   | any        | `Pending`     |
/// | `Blocked`   | any        | `Blocked`     |
/// | `Cancelled` | any        | `Cancelled`   |
///
/// Staleness only refines the terminal outcomes `Passed` and `Failed`. The
/// non-outcome states (`Running`, `Pending`, `Blocked`) and the terminal
/// `Cancelled` ignore `is_stale` entirely — in particular `Cancelled` is
/// terminal-and-non-stale, so there is no `cancelled_stale` token.
///
/// # Examples
///
/// ```
/// use spork_status::{effective_status, EffectiveStatus, Lifecycle};
///
/// assert_eq!(effective_status(Lifecycle::Passed, false), EffectiveStatus::Green);
/// assert_eq!(effective_status(Lifecycle::Failed, true), EffectiveStatus::RedStale);
/// assert_eq!(effective_status(Lifecycle::Running, true), EffectiveStatus::Running);
/// assert_eq!(effective_status(Lifecycle::Cancelled, true), EffectiveStatus::Cancelled);
/// ```
#[must_use]
pub fn effective_status(status: Lifecycle, is_stale: bool) -> EffectiveStatus {
    match status {
        Lifecycle::Passed => {
            if is_stale {
                EffectiveStatus::GreenStale
            } else {
                EffectiveStatus::Green
            }
        }
        Lifecycle::Failed => {
            if is_stale {
                EffectiveStatus::RedStale
            } else {
                EffectiveStatus::Red
            }
        }
        // Staleness does not apply to non-outcome or cancelled states.
        Lifecycle::Running => EffectiveStatus::Running,
        Lifecycle::Pending => EffectiveStatus::Pending,
        Lifecycle::Blocked => EffectiveStatus::Blocked,
        Lifecycle::Cancelled => EffectiveStatus::Cancelled,
    }
}

/// Errors arising when interpreting a status value from an untrusted source.
///
/// `#[non_exhaustive]` so new variants can be added additively (constraint C2)
/// without breaking downstream `match`es. F2 has a single variant; it exists so
/// callers parsing a status token from the wire have a typed error to surface.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StatusError {
    /// A status token string did not correspond to any known lifecycle state.
    #[error("unknown lifecycle status token: {token:?}")]
    UnknownLifecycle {
        /// The unrecognized token as received.
        token: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full Cartesian product of `(Lifecycle, is_stale)` — every input to
    /// the total fold.
    fn all_inputs() -> Vec<(Lifecycle, bool)> {
        let mut v = Vec::with_capacity(Lifecycle::ALL.len() * 2);
        for &l in &Lifecycle::ALL {
            v.push((l, false));
            v.push((l, true));
        }
        v
    }

    #[test]
    fn lifecycle_all_is_complete_and_in_order() {
        // The declared ALL array must enumerate every variant exactly once.
        assert_eq!(Lifecycle::ALL.len(), 6);
        assert_eq!(
            Lifecycle::ALL,
            [
                Lifecycle::Pending,
                Lifecycle::Running,
                Lifecycle::Passed,
                Lifecycle::Failed,
                Lifecycle::Blocked,
                Lifecycle::Cancelled,
            ]
        );
    }

    #[test]
    fn effective_status_all_is_complete_and_in_order() {
        assert_eq!(EffectiveStatus::ALL.len(), 8);
        assert_eq!(
            EffectiveStatus::ALL,
            [
                EffectiveStatus::Green,
                EffectiveStatus::GreenStale,
                EffectiveStatus::Red,
                EffectiveStatus::RedStale,
                EffectiveStatus::Running,
                EffectiveStatus::Pending,
                EffectiveStatus::Blocked,
                EffectiveStatus::Cancelled,
            ]
        );
    }

    #[test]
    fn fold_is_total_one_token_per_input() {
        // Every (Lifecycle, is_stale) yields a token, and the token is one of
        // the eight declared ones. There are 6 * 2 = 12 inputs.
        let inputs = all_inputs();
        assert_eq!(inputs.len(), 12);
        for (l, s) in inputs {
            let token = effective_status(l, s);
            assert!(
                EffectiveStatus::ALL.contains(&token),
                "fold produced an out-of-set token for ({l:?}, {s})"
            );
        }
    }

    #[test]
    fn fold_exact_mapping_exhaustive() {
        use EffectiveStatus as E;
        use Lifecycle as L;
        // The complete truth table, spelled out independently of the impl.
        let expected: [(L, bool, E); 12] = [
            (L::Passed, false, E::Green),
            (L::Passed, true, E::GreenStale),
            (L::Failed, false, E::Red),
            (L::Failed, true, E::RedStale),
            (L::Running, false, E::Running),
            (L::Running, true, E::Running),
            (L::Pending, false, E::Pending),
            (L::Pending, true, E::Pending),
            (L::Blocked, false, E::Blocked),
            (L::Blocked, true, E::Blocked),
            (L::Cancelled, false, E::Cancelled),
            (L::Cancelled, true, E::Cancelled),
        ];
        for (l, s, want) in expected {
            assert_eq!(
                effective_status(l, s),
                want,
                "({l:?}, is_stale={s}) should fold to {want:?}"
            );
        }
    }

    #[test]
    fn cancelled_maps_to_cancelled_regardless_of_stale() {
        // The A.7 fix: cancelled IS a token and is terminal-and-non-stale.
        assert_eq!(
            effective_status(Lifecycle::Cancelled, false),
            EffectiveStatus::Cancelled
        );
        assert_eq!(
            effective_status(Lifecycle::Cancelled, true),
            EffectiveStatus::Cancelled
        );
    }

    #[test]
    fn there_is_no_cancelled_stale_token() {
        // No effective-status token may itself be both "cancelled-like" and
        // stale: Cancelled is never stale, and no stale token is Cancelled.
        assert!(!EffectiveStatus::Cancelled.is_stale());
        for &t in &EffectiveStatus::ALL {
            if t.is_stale() {
                assert!(
                    matches!(t, EffectiveStatus::GreenStale | EffectiveStatus::RedStale),
                    "only Green/Red have stale companions"
                );
            }
        }
    }

    #[test]
    fn staleness_only_affects_passed_and_failed() {
        // For every lifecycle, flipping is_stale changes the token iff the
        // state is Passed or Failed.
        for &l in &Lifecycle::ALL {
            let not_stale = effective_status(l, false);
            let stale = effective_status(l, true);
            let should_differ = matches!(l, Lifecycle::Passed | Lifecycle::Failed);
            assert_eq!(
                not_stale != stale,
                should_differ,
                "staleness sensitivity wrong for {l:?}"
            );
        }
    }

    #[test]
    fn non_stale_token_when_input_not_stale() {
        // When is_stale is false, the produced token must never be a *Stale one.
        for &l in &Lifecycle::ALL {
            let token = effective_status(l, false);
            assert!(
                !token.is_stale(),
                "non-stale input {l:?} produced stale token {token:?}"
            );
        }
    }

    #[test]
    fn terminal_classification() {
        assert!(Lifecycle::Passed.is_terminal());
        assert!(Lifecycle::Failed.is_terminal());
        assert!(Lifecycle::Cancelled.is_terminal());
        assert!(!Lifecycle::Pending.is_terminal());
        assert!(!Lifecycle::Running.is_terminal());
        assert!(!Lifecycle::Blocked.is_terminal());
    }

    #[test]
    fn lifecycle_serde_roundtrip_snake_case() {
        for &l in &Lifecycle::ALL {
            let json = serde_json::to_string(&l).expect("serialize");
            let back: Lifecycle = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(l, back);
        }
        // Spot-check the exact wire form is snake_case.
        assert_eq!(
            serde_json::to_string(&Lifecycle::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::from_str::<Lifecycle>("\"passed\"").unwrap(),
            Lifecycle::Passed
        );
    }

    #[test]
    fn effective_status_serde_roundtrip_snake_case() {
        for &t in &EffectiveStatus::ALL {
            let json = serde_json::to_string(&t).expect("serialize");
            let back: EffectiveStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(t, back);
        }
        assert_eq!(
            serde_json::to_string(&EffectiveStatus::GreenStale).unwrap(),
            "\"green_stale\""
        );
        assert_eq!(
            serde_json::to_string(&EffectiveStatus::RedStale).unwrap(),
            "\"red_stale\""
        );
    }

    #[test]
    fn status_error_display() {
        let e = StatusError::UnknownLifecycle {
            token: "bogus".to_string(),
        };
        assert!(e.to_string().contains("bogus"));
    }

    #[test]
    fn schema_version_is_one() {
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
