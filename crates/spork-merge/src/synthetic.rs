//! The [`SyntheticTranscript`] — conversation merge, made concrete.
//!
//! Conversation transcripts have no clean three-way analogue, so conversation
//! merge is necessarily **append-style** (DESIGN.md §6.5). DESIGN.md A.4 makes
//! that concrete: the merged Edit node's conversation ref points to a synthetic
//! transcript =
//!
//! > ancestor-common-prefix + a generated merge-summary block + both branch
//! > tails marked with provenance tags.
//!
//! It is explicitly flagged `lossyProjection`-adjacent: the agent is told this
//! is a *merged* context, not a linear one. This module freezes that schema
//! (CLAUDE.md C5) and provides [`synthesize`], a deterministic builder so the
//! result is content-addressable (DESIGN.md §6.1) — byte-identical inputs always
//! yield a byte-identical synthetic transcript that hashes stably.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use spork_provider::CanonicalTurn;

use crate::MergeSchemaError;

/// The frozen schema version of [`SyntheticTranscript`] (CLAUDE.md C5).
///
/// Persisted and hashed alongside the transcript, so the shape can evolve
/// additively in a later generation without silently reinterpreting an existing
/// stored merge conversation (the no-domino seam, DESIGN.md A.7).
pub const SYNTHETIC_TRANSCRIPT_SCHEMA_VERSION: u16 = 1;

/// The provenance source label for the "ours" branch tail.
///
/// Keeping the two source labels crate constants means [`synthesize`] and any
/// downstream reader agree on one spelling, and the provenance tags are stable
/// across runs (DESIGN.md A.4: tails are "marked with provenance tags").
pub const SOURCE_OURS: &str = "ours";

/// The provenance source label for the "theirs" branch tail.
pub const SOURCE_THEIRS: &str = "theirs";

/// One branch's diverging turns, tagged with the branch they came from.
///
/// The synthetic transcript appends both branch tails after the common prefix
/// and the merge summary; each tail is labeled with its `source` provenance so a
/// reader (and the agent) can tell which branch a turn came from in the merged,
/// non-linear context (DESIGN.md §6.5, A.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceTaggedTail {
    /// The branch this tail came from (e.g. [`SOURCE_OURS`] / [`SOURCE_THEIRS`]).
    pub source: String,
    /// The diverging turns of this branch, in order, after the common prefix.
    pub turns: Vec<CanonicalTurn>,
}

/// The append-style merge of two conversation transcripts.
///
/// Built from a common-ancestor transcript and the two branch transcripts: the
/// `common_prefix` is the shared history, `merge_summary` is the generated
/// "this is a merged context" note, and `tails` carries each branch's diverging
/// turns tagged with provenance (DESIGN.md §6.5, A.4). It is schema-versioned
/// (CLAUDE.md C5) and content-addressable (DESIGN.md §6.1): two byte-identical
/// merges hash identically and dedup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticTranscript {
    /// The schema version of this transcript shape.
    pub schema_version: u16,
    /// The shared ancestor history, before either branch diverged.
    pub common_prefix: Vec<CanonicalTurn>,
    /// The generated merge-summary note flagging this as a merged context.
    pub merge_summary: String,
    /// Each branch's diverging turns, provenance-tagged, in a deterministic
    /// order ([`SOURCE_OURS`] then [`SOURCE_THEIRS`]).
    pub tails: Vec<ProvenanceTaggedTail>,
}

impl SyntheticTranscript {
    /// The content-address hash of this transcript over its canonical bytes.
    ///
    /// Two byte-identical synthetic transcripts hash identically and dedup; the
    /// hash is the identity the merged Edit node binds its conversation ref to,
    /// so the merged conversation restores alongside its code (DESIGN.md §6.1,
    /// §6.5).
    ///
    /// # Errors
    /// Returns [`MergeSchemaError::Canon`] only if the transcript somehow
    /// contains a value the canonical encoder rejects (a float). The canonical
    /// transcript schema is float-free, so in practice this does not fire.
    pub fn content_hash(&self) -> Result<Hash, MergeSchemaError> {
        let bytes = spork_canon::canonicalize(self)?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

/// Compute how many leading turns `branch` shares with `common`, by value.
///
/// The common prefix is the longest run of turns that are equal (whole-turn,
/// structural equality via `PartialEq`) at the same position in both `common`
/// and `branch`. Everything in `branch` past that length is the branch's
/// diverging tail. This is the obvious, deterministic notion of a shared prefix
/// for an append-style merge; it does not attempt a token-level diff.
fn shared_prefix_len(common: &[CanonicalTurn], branch: &[CanonicalTurn]) -> usize {
    common
        .iter()
        .zip(branch.iter())
        .take_while(|(c, b)| c == b)
        .count()
}

/// Deterministically build a [`SyntheticTranscript`] from three transcripts.
///
/// `common` is the nearest-common-ancestor conversation; `ours` and `theirs` are
/// the two branch conversations being merged; `summary` is the generated
/// merge-summary note. The result is the append-style merge of DESIGN.md §6.5 /
/// A.4:
///
/// - `common_prefix` = the turns each branch actually shares with `common`
///   (the shorter of the two branches' shared prefixes, so the prefix is one
///   both branches genuinely agree on);
/// - `merge_summary` = `summary` verbatim;
/// - `tails` = each branch's turns *after* that common prefix, tagged
///   [`SOURCE_OURS`] then [`SOURCE_THEIRS`] in that fixed order.
///
/// The function is **pure and deterministic**: it reads only its inputs, does no
/// I/O, allocates the same output for the same input every time, and the tail
/// order is fixed — so the resulting transcript content-addresses stably
/// (DESIGN.md §6.1). It never panics and clones rather than mutating its inputs.
///
/// # Example
/// ```
/// use spork_merge::{synthesize, SOURCE_OURS, SOURCE_THEIRS};
/// use spork_provider::{CanonicalTranscript, CanonicalTurn, Role};
///
/// let common = CanonicalTranscript::new(vec![CanonicalTurn::text(Role::User, "task")]);
/// let ours = CanonicalTranscript::new(vec![
///     CanonicalTurn::text(Role::User, "task"),
///     CanonicalTurn::text(Role::Assistant, "did it our way"),
/// ]);
/// let theirs = CanonicalTranscript::new(vec![
///     CanonicalTurn::text(Role::User, "task"),
///     CanonicalTurn::text(Role::Assistant, "did it their way"),
/// ]);
///
/// let merged = synthesize(&common, &ours, &theirs, "merged".into());
/// assert_eq!(merged.common_prefix.len(), 1);
/// assert_eq!(merged.merge_summary, "merged");
/// assert_eq!(merged.tails[0].source, SOURCE_OURS);
/// assert_eq!(merged.tails[1].source, SOURCE_THEIRS);
/// assert_eq!(merged.tails[0].turns.len(), 1);
/// ```
#[must_use]
pub fn synthesize(
    common: &spork_provider::CanonicalTranscript,
    ours: &spork_provider::CanonicalTranscript,
    theirs: &spork_provider::CanonicalTranscript,
    summary: String,
) -> SyntheticTranscript {
    // The shared prefix is the part *both* branches agree on with the common
    // ancestor, so we take the shorter of the two per-branch shared prefixes.
    // (If one branch rewrote earlier history, the prefix shrinks to what both
    // still share — never asserting agreement that isn't there.)
    let prefix_len = shared_prefix_len(&common.turns, &ours.turns)
        .min(shared_prefix_len(&common.turns, &theirs.turns));

    let common_prefix = common.turns[..prefix_len].to_vec();
    let ours_tail = ours.turns[prefix_len..].to_vec();
    let theirs_tail = theirs.turns[prefix_len..].to_vec();

    SyntheticTranscript {
        schema_version: SYNTHETIC_TRANSCRIPT_SCHEMA_VERSION,
        common_prefix,
        merge_summary: summary,
        // Fixed order: ours then theirs, so the output is deterministic.
        tails: vec![
            ProvenanceTaggedTail {
                source: SOURCE_OURS.to_string(),
                turns: ours_tail,
            },
            ProvenanceTaggedTail {
                source: SOURCE_THEIRS.to_string(),
                turns: theirs_tail,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_provider::{CanonicalTranscript, ContentBlock, Role};

    fn tx(turns: Vec<CanonicalTurn>) -> CanonicalTranscript {
        CanonicalTranscript::new(turns)
    }

    fn divergent() -> (
        CanonicalTranscript,
        CanonicalTranscript,
        CanonicalTranscript,
    ) {
        let common = tx(vec![
            CanonicalTurn::text(Role::System, "be helpful"),
            CanonicalTurn::text(Role::User, "do the task"),
        ]);
        let ours = tx(vec![
            CanonicalTurn::text(Role::System, "be helpful"),
            CanonicalTurn::text(Role::User, "do the task"),
            CanonicalTurn::text(Role::Assistant, "ours-1"),
            CanonicalTurn::text(Role::Assistant, "ours-2"),
        ]);
        let theirs = tx(vec![
            CanonicalTurn::text(Role::System, "be helpful"),
            CanonicalTurn::text(Role::User, "do the task"),
            CanonicalTurn::text(Role::Assistant, "theirs-1"),
        ]);
        (common, ours, theirs)
    }

    #[test]
    fn splits_common_prefix_and_provenance_tagged_tails() {
        let (common, ours, theirs) = divergent();
        let s = synthesize(&common, &ours, &theirs, "summary".into());

        assert_eq!(s.schema_version, SYNTHETIC_TRANSCRIPT_SCHEMA_VERSION);
        assert_eq!(s.common_prefix.len(), 2);
        assert_eq!(s.merge_summary, "summary");

        assert_eq!(s.tails.len(), 2);
        assert_eq!(s.tails[0].source, SOURCE_OURS);
        assert_eq!(s.tails[1].source, SOURCE_THEIRS);

        // The tails are exactly each branch's turns past the common prefix.
        assert_eq!(s.tails[0].turns.len(), 2);
        assert_eq!(
            s.tails[0].turns[0].content,
            vec![ContentBlock::Text("ours-1".into())]
        );
        assert_eq!(s.tails[1].turns.len(), 1);
        assert_eq!(
            s.tails[1].turns[0].content,
            vec![ContentBlock::Text("theirs-1".into())]
        );
    }

    #[test]
    fn is_deterministic() {
        let (common, ours, theirs) = divergent();
        let a = synthesize(&common, &ours, &theirs, "s".into());
        let b = synthesize(&common, &ours, &theirs, "s".into());
        assert_eq!(a, b);
        // ...down to the content-address.
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn tail_order_is_fixed_so_swapping_branches_changes_identity() {
        // Provenance is meaningful: ours/theirs are NOT interchangeable, so
        // swapping them must produce a different synthetic transcript. This
        // guards the "marked with provenance tags" contract (DESIGN.md A.4).
        let (common, ours, theirs) = divergent();
        let normal = synthesize(&common, &ours, &theirs, "s".into());
        let swapped = synthesize(&common, &theirs, &ours, "s".into());
        assert_ne!(normal, swapped);
        assert_ne!(
            normal.content_hash().unwrap(),
            swapped.content_hash().unwrap()
        );
    }

    #[test]
    fn content_hash_is_stable_across_clones() {
        let (common, ours, theirs) = divergent();
        let s = synthesize(&common, &ours, &theirs, "s".into());
        let cloned = s.clone();
        assert_eq!(s.content_hash().unwrap(), cloned.content_hash().unwrap());
        // And matches a manual canonical hash.
        let bytes = spork_canon::canonicalize(&s).unwrap();
        assert_eq!(s.content_hash().unwrap(), spork_hash::hash_bytes(&bytes));
    }

    #[test]
    fn diverging_summary_changes_hash() {
        let (common, ours, theirs) = divergent();
        let a = synthesize(&common, &ours, &theirs, "first".into());
        let b = synthesize(&common, &ours, &theirs, "second".into());
        assert_ne!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn prefix_shrinks_when_a_branch_rewrote_shared_history() {
        // `theirs` diverges already at turn index 1, so the prefix both branches
        // genuinely share is just the first turn — not the full common ancestor.
        let common = tx(vec![
            CanonicalTurn::text(Role::System, "sys"),
            CanonicalTurn::text(Role::User, "u"),
        ]);
        let ours = common.clone();
        let theirs = tx(vec![
            CanonicalTurn::text(Role::System, "sys"),
            CanonicalTurn::text(Role::User, "REWRITTEN"),
        ]);
        let s = synthesize(&common, &ours, &theirs, "s".into());
        assert_eq!(s.common_prefix.len(), 1);
        // ours kept the original second turn in its tail; theirs has the rewrite.
        assert_eq!(s.tails[0].turns.len(), 1);
        assert_eq!(s.tails[1].turns.len(), 1);
    }

    #[test]
    fn identical_branches_yield_empty_tails() {
        let (common, _, _) = divergent();
        let same = common.clone();
        let s = synthesize(&common, &same, &same, "noop".into());
        assert_eq!(s.common_prefix.len(), common.turns.len());
        assert!(s.tails[0].turns.is_empty());
        assert!(s.tails[1].turns.is_empty());
    }

    #[test]
    fn serde_round_trips_with_opaque_and_tool_blocks() {
        // Tails can carry the full canonical content surface, including tool
        // calls/results, and still round-trip + hash.
        let common = tx(vec![CanonicalTurn::text(Role::User, "go")]);
        let ours = tx(vec![
            CanonicalTurn::text(Role::User, "go"),
            CanonicalTurn {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "search".into(),
                    input: serde_json::json!({"q": "x"}),
                }],
                tool_call_id: None,
                opaque: Vec::new(),
            },
        ]);
        let theirs = tx(vec![CanonicalTurn::text(Role::User, "go")]);
        let s = synthesize(&common, &ours, &theirs, "merged".into());

        let json = serde_json::to_string(&s).unwrap();
        let back: SyntheticTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        assert_eq!(s.content_hash().unwrap(), back.content_hash().unwrap());
    }
}
