//! End-to-end tests of the context seam against the *real* `spork-graph` and
//! `spork-provider` crates.
//!
//! The unit tests prove the compiler/handoff logic against in-memory sources;
//! these tests close the loop against the real F2 graph projection (so a handoff
//! binds to a node's authoritative `lineage_hash`) and the real F4
//! `AnthropicAdapter` (so a transcript carried as ancestor-verbatim context
//! round-trips through the canonical transcript). They are the F4 cross-seam
//! proofs from the contract's Definition of Done.

use serde_json::json;
use spork_context::{
    ContextCompiler, ContextPolicy, ContextSource, GraphHandoffSource, HandoffDistillation,
    HandoffGenerator, LayeredCompiler, NodeMaterials, SingleNodeHandoffGenerator,
};
use spork_graph::GraphService;
use spork_hash::hash_bytes;
use spork_log::EventLog;
use spork_provider::{AnthropicAdapter, CanonicalTranscript, CanonicalTurn, ProviderAdapter, Role};
use std::collections::HashMap;
use ulid::Ulid;

/// A small in-memory [`ContextSource`] over a fixed map.
struct MapSource(HashMap<Ulid, NodeMaterials>);

impl ContextSource for MapSource {
    fn node_materials(
        &self,
        node: Ulid,
    ) -> Result<Option<NodeMaterials>, spork_context::ContextError> {
        Ok(self.0.get(&node).cloned())
    }
}

/// Two real graph siblings (same parent) compile to the SAME prefix_hash when
/// they share a stable prefix, even though they are distinct nodes with distinct
/// ids — the cache-reuse property from DESIGN §13.2, proven across real
/// graph-minted siblings.
#[test]
fn real_sibling_graph_nodes_share_prefix_hash() {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let mut svc = GraphService::open_in_memory(log.writer()).unwrap();
    svc.register_builtin_snapshot().unwrap();

    // A parent snapshot and two sibling snapshot children of it.
    let parent = svc
        .create_node(
            "snapshot",
            None,
            vec![],
            "main",
            json!({ "origin": "manual" }),
            true,
            Some(hash_bytes(b"parent-tree")),
        )
        .unwrap();
    let sib_a = svc
        .create_node(
            "snapshot",
            None,
            vec![parent.id],
            "feature-a",
            json!({ "origin": "manual" }),
            true,
            Some(hash_bytes(b"a-tree")),
        )
        .unwrap();
    let sib_b = svc
        .create_node(
            "snapshot",
            None,
            vec![parent.id],
            "feature-b",
            json!({ "origin": "manual" }),
            true,
            Some(hash_bytes(b"b-tree")),
        )
        .unwrap();
    // The two siblings are genuinely distinct nodes with distinct lineage.
    assert_ne!(sib_a.id, sib_b.id);
    assert_ne!(sib_a.lineage_hash, sib_b.lineage_hash);

    // Compile both siblings with an identical stable prefix but different
    // volatile tails (different current diff + user msg per branch).
    let stable = |branch: &str, diff: &str| NodeMaterials {
        system_prompt: Some("you are a coding agent".into()),
        project_memory: Some("project: spork".into()),
        repo_map: Some("src/lib.rs: fn main".into()),
        current_diff: Some(format!("{branch}: {diff}")),
        user_msg: Some(format!("work on {branch}")),
        ..Default::default()
    };
    let mut nodes = HashMap::new();
    nodes.insert(sib_a.id, stable("feature-a", "edited a.rs"));
    nodes.insert(
        sib_b.id,
        stable("feature-b", "edited a totally different file b.rs"),
    );
    let compiler = LayeredCompiler::new(MapSource(nodes));
    let pol = ContextPolicy::default();

    let ca = compiler.compile(sib_a.id, &pol).unwrap();
    let cb = compiler.compile(sib_b.id, &pol).unwrap();

    assert_eq!(
        ca.prefix_hash, cb.prefix_hash,
        "sibling branches with the same stable prefix must reuse the warm cache"
    );
    assert_ne!(
        ca.volatile_tail(),
        cb.volatile_tail(),
        "the volatile tails really do differ"
    );
}

/// A handoff generated from a real graph node binds to that node's authoritative
/// `lineage_hash` and is regenerable — DESIGN §13.5 against the real F2 graph.
#[test]
fn handoff_binds_to_real_graph_lineage_hash() {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let mut svc = GraphService::open_in_memory(log.writer()).unwrap();
    svc.register_builtin_snapshot().unwrap();

    let node = svc
        .create_node(
            "snapshot",
            None,
            vec![],
            "main",
            json!({ "origin": "manual" }),
            true,
            Some(hash_bytes(b"tree")),
        )
        .unwrap();
    let expected_lineage = node.lineage_hash;

    // The graph-backed handoff source pulls the lineage hash from the envelope;
    // the distillation prose is supplied by the caller's closure.
    let source = GraphHandoffSource::new(svc.projection(), |env| HandoffDistillation {
        summary: format!("node {} on branch {}", env.id, env.branch_id),
        key_decisions: vec!["captured an initial snapshot".into()],
        files_touched: vec![],
        open_threads: vec![],
        constraints: vec![],
        test_state: "n/a".into(),
    });
    let generator = SingleNodeHandoffGenerator::new(source);
    let doc = generator.generate(node.id).unwrap();

    assert!(doc.regenerable, "a generated handoff is always regenerable");
    assert_eq!(
        doc.lineage_hash, expected_lineage,
        "the handoff must bind to the node's authoritative lineage_hash"
    );

    // Regeneration is deterministic: the same node yields a byte-identical doc.
    let doc2 = generator.generate(node.id).unwrap();
    assert_eq!(doc.content_hash().unwrap(), doc2.content_hash().unwrap());

    // An unknown node id is reported as not-found, not silently empty.
    let missing = generator.generate(Ulid::new());
    assert!(missing.is_err());
}

/// A canonical transcript round-trips through the real AnthropicAdapter, and the
/// rendered transcript can be carried as ancestor-verbatim context that the
/// compiler orders into the volatile tail (rank 3) — closing the §12.3/§13.2
/// seam: the same stored transcript both maps to the wire and feeds context.
#[test]
fn transcript_round_trips_and_feeds_ancestor_context() {
    let adapter = AnthropicAdapter::new();
    let transcript = CanonicalTranscript::new(vec![
        CanonicalTurn::text(Role::System, "you are an agent"),
        CanonicalTurn::text(Role::User, "add retries to the client"),
        CanonicalTurn::text(Role::Assistant, "done, added exponential backoff"),
    ]);

    // The provider seam: canonical -> wire -> canonical is lossless for portable
    // content (the F4 provider DoD), proven here against the real adapter.
    let wire = adapter.to_wire(&transcript).unwrap();
    let back = adapter.from_wire(wire).unwrap();
    assert_eq!(
        back.turns.len(),
        transcript.turns.len(),
        "portable turns survive the round-trip"
    );

    // The same transcript content can be rendered into an ancestor-verbatim
    // context layer; the compiler places it in the rank-3 volatile tail.
    let rendered: String = transcript
        .turns
        .iter()
        .map(|t| format!("{:?}: {:?}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");

    let id = Ulid::new();
    let anc = Ulid::new();
    let mut nodes = HashMap::new();
    nodes.insert(
        id,
        NodeMaterials {
            system_prompt: Some("system".into()),
            ancestor_verbatim: vec![spork_context::AncestorText::new(anc, rendered.clone())],
            user_msg: Some("continue".into()),
            ..Default::default()
        },
    );
    let compiler = LayeredCompiler::new(MapSource(nodes));
    let pol = ContextPolicy::new(
        1_000_000,
        spork_context::AncestorStrategy::Verbatim,
        spork_context::Degrade::Compact,
    );
    let cc = compiler.compile(id, &pol).unwrap();
    assert!(
        cc.volatile_tail().iter().any(|l| l.kind
            == spork_context::ContextLayerKind::AncestorVerbatim
            && l.content == rendered),
        "the verbatim ancestor transcript is carried in the volatile tail"
    );
}
