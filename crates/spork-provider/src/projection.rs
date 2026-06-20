//! The [`ProviderProjection`] layer over a single canonical transcript.
//!
//! Hot-swapping a model mid-session re-renders the stored canonical transcript
//! into the target provider's wire format. Provider-specific artifacts that do
//! not map across vendors — Claude extended-thinking blocks, OpenAI reasoning
//! tokens — are stored as [`OpaqueProviderBlock`](crate::OpaqueProviderBlock)s
//! and **dropped on a cross-provider projection with an explicit
//! `lossyProjection` warning recorded on the turn**, because a chain-of-thought
//! the next turn depended on cannot be faithfully carried across a swap
//! (DESIGN.md §12.3).
//!
//! Projection here is the *pre-wire* step: it transforms one
//! [`CanonicalTranscript`](crate::CanonicalTranscript) into the canonical
//! transcript appropriate for a `target_provider` (dropping foreign opaque
//! blocks), recording each drop as a [`LossyProjection`] warning. The resulting
//! transcript is then handed to that provider's
//! [`ProviderAdapter::to_wire`](crate::ProviderAdapter::to_wire). A
//! *same-provider* projection is the identity on content and produces no
//! warnings.

use crate::{CanonicalTranscript, CanonicalTurn};

/// One recorded loss from a cross-provider projection.
///
/// Each entry names the turn index and the provider whose opaque block was
/// dropped, so the agent can be told this turn's history is lossy (the
/// `lossyProjection` warning on the turn, DESIGN.md §12.3). It serializes so the
/// warning can be persisted on the node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LossyProjection {
    /// Index of the turn (in the source transcript) that lost a block.
    pub turn_index: usize,
    /// The provider key of the dropped opaque block.
    pub dropped_provider: String,
    /// A stable machine-readable warning code: always `"lossyProjection"`.
    pub warning: String,
}

impl LossyProjection {
    /// The frozen warning code recorded for every cross-provider drop.
    pub const CODE: &'static str = "lossyProjection";

    fn new(turn_index: usize, dropped_provider: impl Into<String>) -> Self {
        LossyProjection {
            turn_index,
            dropped_provider: dropped_provider.into(),
            warning: Self::CODE.to_string(),
        }
    }
}

/// The result of projecting a transcript onto a target provider.
///
/// Holds the projected, provider-appropriate transcript plus every
/// [`LossyProjection`] warning incurred. An empty `warnings` list means the
/// projection was lossless (same-provider, or no foreign opaque blocks present).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projected {
    /// The transcript re-rendered for the target provider, with foreign opaque
    /// blocks removed.
    pub transcript: CanonicalTranscript,
    /// The losses incurred, one per dropped foreign opaque block.
    pub warnings: Vec<LossyProjection>,
}

impl Projected {
    /// Whether this projection dropped anything (i.e. recorded any
    /// `lossyProjection` warning).
    #[must_use]
    pub fn is_lossy(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Re-renders a canonical transcript for a target provider.
///
/// Stateless; the entry point is [`ProviderProjection::project`]. F4 ships the
/// one real projection behavior (dropping foreign opaque blocks with a recorded
/// warning); the projection seam itself does not change in later phases.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderProjection;

impl ProviderProjection {
    /// Construct the projector.
    #[must_use]
    pub fn new() -> Self {
        ProviderProjection
    }

    /// Project `transcript` onto `target_provider`.
    ///
    /// Portable [`ContentBlock`](crate::ContentBlock)s are preserved unchanged.
    /// Each [`OpaqueProviderBlock`](crate::OpaqueProviderBlock) is kept iff its
    /// `provider` equals `target_provider`; a block tagged for any other
    /// provider is dropped and a [`LossyProjection`] warning is recorded against
    /// its turn index. A same-provider projection therefore keeps every block
    /// and records no warnings.
    ///
    /// # Example
    /// ```
    /// use spork_provider::{
    ///     CanonicalTranscript, CanonicalTurn, ContentBlock, OpaqueProviderBlock,
    ///     ProviderProjection, Role,
    /// };
    /// use serde_json::json;
    ///
    /// let turn = CanonicalTurn {
    ///     role: Role::Assistant,
    ///     content: vec![ContentBlock::Text("hi".into())],
    ///     tool_call_id: None,
    ///     opaque: vec![OpaqueProviderBlock {
    ///         provider: "anthropic".into(),
    ///         data: json!({"type": "thinking", "thinking": "…"}),
    ///     }],
    /// };
    /// let t = CanonicalTranscript::new(vec![turn]);
    ///
    /// // Projecting to a *different* provider drops the anthropic block.
    /// let p = ProviderProjection::new().project(&t, "openai");
    /// assert!(p.is_lossy());
    /// assert_eq!(p.warnings[0].dropped_provider, "anthropic");
    /// assert!(p.transcript.turns[0].opaque.is_empty());
    ///
    /// // Projecting back to anthropic keeps it and warns nothing.
    /// let p = ProviderProjection::new().project(&t, "anthropic");
    /// assert!(!p.is_lossy());
    /// assert_eq!(p.transcript.turns[0].opaque.len(), 1);
    /// ```
    #[must_use]
    pub fn project(&self, transcript: &CanonicalTranscript, target_provider: &str) -> Projected {
        let mut warnings = Vec::new();
        let turns = transcript
            .turns
            .iter()
            .enumerate()
            .map(|(i, turn)| {
                let mut kept = Vec::with_capacity(turn.opaque.len());
                for block in &turn.opaque {
                    if block.provider == target_provider {
                        kept.push(block.clone());
                    } else {
                        warnings.push(LossyProjection::new(i, block.provider.clone()));
                    }
                }
                CanonicalTurn {
                    role: turn.role,
                    content: turn.content.clone(),
                    tool_call_id: turn.tool_call_id.clone(),
                    opaque: kept,
                }
            })
            .collect();

        Projected {
            transcript: CanonicalTranscript {
                schema_version: transcript.schema_version,
                turns,
            },
            warnings,
        }
    }
}
