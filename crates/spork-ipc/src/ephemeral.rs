//! The ephemeral side-channel contract — [`EphemeralFrame`] and
//! [`EphemeralChannel`].
//!
//! High-frequency, low-durability data (agent chat tokens, test/run stdout) does
//! **not** ride the ordered op-log. It travels on lightweight side-channels keyed
//! by node id, so a flood of tokens or stdout can never stall durable
//! [`OpLogEvent`](crate::OpLogEvent) delivery (DESIGN.md §5.5, §14.4). This
//! module freezes the *shape* of those frames; the transport that keeps the two
//! channels independent lives in `spork-stream`.
//!
//! The defining contrast with [`OpLogEvent`](crate::OpLogEvent):
//! - An `OpLogEvent` is **durable and ordered** — it is part of the source of
//!   truth, carries a monotonic `seq`, and is reduced into graph state.
//! - An [`EphemeralFrame`] is **transient and unordered across nodes** — it is
//!   keyed by `node_id`, carries no `seq`, is never persisted to the log, and is
//!   safe to drop or coalesce under backpressure without affecting correctness.
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.4.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// The schema version of the [`EphemeralFrame`] envelope. See
/// [`COMMAND_SCHEMA_VERSION`](crate::COMMAND_SCHEMA_VERSION) for the evolution
/// discipline; this is additive like the rest of the contract.
pub const EPHEMERAL_FRAME_SCHEMA_VERSION: u16 = 1;

/// Which ephemeral side-channel a frame belongs to.
///
/// A frozen, fixed vocabulary of the high-frequency streams Spork multiplexes
/// off the durable op-log (DESIGN.md §5.5, §14.4). A renderer subscribes per
/// `(node_id, channel)` so chat tokens and run output stay distinct surfaces in
/// the Node-Details panel and bottom run rail (DESIGN.md §14.2).
///
/// Serialized as a `SCREAMING_SNAKE_CASE` string tag so the wire form is stable
/// independent of source order; new channels are appended additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EphemeralChannel {
    /// Streaming agent chat/completion tokens for a node's conversation.
    ChatTokens,
    /// Streaming stdout/stderr of a running check/command for a node.
    RunStdout,
}

impl EphemeralChannel {
    /// Every channel, for exhaustive iteration in tests and subscriptions.
    pub const ALL: [EphemeralChannel; 2] =
        [EphemeralChannel::ChatTokens, EphemeralChannel::RunStdout];

    /// The stable wire tag of this channel (e.g. `"CHAT_TOKENS"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EphemeralChannel::ChatTokens => "CHAT_TOKENS",
            EphemeralChannel::RunStdout => "RUN_STDOUT",
        }
    }
}

/// One frame of high-frequency, transient data for a node.
///
/// Keyed by `node_id` and tagged with its [`EphemeralChannel`], a frame carries
/// a chunk of `data` (a token batch, a line of stdout). It deliberately has
/// **no `seq`**: ephemeral frames are not part of the durable, ordered source of
/// truth and may be dropped or coalesced under backpressure (DESIGN.md §5.5,
/// §14.4). The transport guarantee — that frames never stall ordered
/// [`OpLogEvent`](crate::OpLogEvent) delivery — is enforced in `spork-stream`.
///
/// Fields are `camelCase` on the wire to match the renderer binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralFrame {
    /// The node this frame belongs to. The renderer routes the frame to that
    /// node's panel/rail; there is no cross-node ordering.
    pub node_id: Ulid,
    /// Which side-channel the frame is on.
    pub channel: EphemeralChannel,
    /// The payload chunk (a batch of tokens or a stdout fragment).
    pub data: String,
}

impl EphemeralFrame {
    /// Construct a frame for a node on a channel.
    #[must_use]
    pub fn new(node_id: Ulid, channel: EphemeralChannel, data: impl Into<String>) -> Self {
        Self {
            node_id,
            channel,
            data: data.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_round_trips_via_stable_tags() {
        for ch in EphemeralChannel::ALL {
            let json = serde_json::to_string(&ch).unwrap();
            let back: EphemeralChannel = serde_json::from_str(&json).unwrap();
            assert_eq!(ch, back);
            // The serialized tag matches `as_str`.
            assert_eq!(json, format!("\"{}\"", ch.as_str()));
        }
    }

    #[test]
    fn frame_round_trips_through_json() {
        let f = EphemeralFrame::new(Ulid::new(), EphemeralChannel::ChatTokens, "hello wor");
        let json = serde_json::to_string(&f).unwrap();
        let back: EphemeralFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn frame_wire_form_is_camel_cased_and_carries_no_seq() {
        let f = EphemeralFrame::new(Ulid::new(), EphemeralChannel::RunStdout, "line\n");
        let v = serde_json::to_value(&f).unwrap();
        assert!(v.get("nodeId").is_some());
        assert_eq!(v["channel"], "RUN_STDOUT");
        assert_eq!(v["data"], "line\n");
        // Ephemeral frames are explicitly NOT part of the ordered stream.
        assert!(
            v.get("seq").is_none(),
            "ephemeral frames must not carry a seq"
        );
    }
}
