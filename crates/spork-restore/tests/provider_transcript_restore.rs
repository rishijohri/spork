//! Cross-seam integration test for the **F4 Definition of Done (c)**:
//!
//! > A chat turn round-trips through `AnthropicAdapter` (`to_wire`/`from_wire`)
//! > and the content-addressed transcript binds to a snapshot and restores with
//! > its code via `spork-restore`.
//!
//! This binds three frozen seams that no single-crate test exercises together:
//!
//! - **F4 provider** (`spork-provider`): a real [`CanonicalTranscript`] is
//!   rendered to Anthropic request JSON by [`AnthropicAdapter::to_wire`] and
//!   parsed back by [`AnthropicAdapter::from_wire`]; the portable content
//!   survives the round trip byte-for-byte at the canonical level (DESIGN §12.1).
//! - **F0 identity** (`spork-canon`/`spork-hash`): the transcript is
//!   content-addressed — its canonical bytes are stored once in the CAS and the
//!   conversation is referenced only by that opaque content hash (DESIGN §6.1).
//! - **F3 restore** (`spork-restore`): a snapshot node binds the code snapshot
//!   *and* that conversation ref; [`RestoreGuard::restore`] materializes the code
//!   byte-identically **and** resolves the bound transcript atomically, then
//!   re-decodes the resolved bytes back into the exact transcript that was bound
//!   (DESIGN §11.4 "restore code + conversation together").
//!
//! `spork-restore` itself stays agnostic of the F4 transcript schema — it holds
//! the conversation as an opaque CAS ref (CLAUDE.md C2). The transcript schema
//! lives entirely in the F4 provider crate; this test wires the two seams the way
//! the daemon will, proving the conversation ref slot frozen in F3 carries a real
//! F4 canonical transcript end to end.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use spork_cas::{LooseStore, ObjectStore};
use spork_graph::{GraphService, BUILTIN_SNAPSHOT_KIND};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_log::EventLog;
use spork_provider::{
    AnthropicAdapter, CanonicalTranscript, CanonicalTurn, ContentBlock, ProviderAdapter, Role,
};
use spork_restore::RestoreGuard;
use tempfile::TempDir;
use ulid::Ulid;

/// Read every regular file under `dir` into a sorted `path -> bytes` map.
fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(base, &path, out);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// A small but realistic chat turn: a system prompt, the user's request, and the
/// assistant's reply — the minimum a single Anthropic chat turn carries.
fn chat_turn() -> CanonicalTranscript {
    CanonicalTranscript::new(vec![
        CanonicalTurn::text(Role::System, "You are a helpful coding assistant."),
        CanonicalTurn::text(Role::User, "Add a greeting to src/main.rs."),
        CanonicalTurn {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(
                "Done — I added a println! greeting to main.".to_string(),
            )],
            tool_call_id: None,
            opaque: vec![],
        },
    ])
}

#[test]
fn chat_turn_round_trips_through_adapter_binds_to_snapshot_and_restores_with_code() {
    let root = TempDir::new().unwrap();
    let cas_dir = root.path().join("cas");
    std::fs::create_dir_all(&cas_dir).unwrap();
    let log = EventLog::open(&root.path().join("log.db")).unwrap();
    let workdir = root.path().join("work");
    let src_dir = root.path().join("src-fixture");
    std::fs::create_dir_all(src_dir.join("src")).unwrap();

    let store = ObjectStore::new(LooseStore::open(&cas_dir).unwrap());

    // ---- Seam 1 (F4 provider): a chat turn round-trips through the adapter. --
    let adapter = AnthropicAdapter::new();
    let original = chat_turn();

    // to_wire renders the canonical transcript into Anthropic request JSON;
    // from_wire parses a response back. Portable content survives losslessly.
    let wire = adapter.to_wire(&original).expect("to_wire");
    let round_tripped = adapter.from_wire(wire).expect("from_wire");
    assert_eq!(
        original, round_tripped,
        "the chat turn must round-trip losslessly through to_wire/from_wire"
    );

    // ---- Seam 2 (F0 identity): the transcript is content-addressed. ---------
    // The conversation's identity is the BLAKE3 hash of its canonical bytes
    // (DESIGN §6.1). The round-tripped transcript content-addresses identically
    // to the original — provider projection does not perturb identity.
    let transcript_hash = original.content_hash().expect("content_hash");
    assert_eq!(
        transcript_hash,
        round_tripped.content_hash().expect("content_hash"),
        "a lossless round trip must preserve the content-address"
    );

    // Store the canonical transcript bytes in the CAS as the conversation blob,
    // and bind the *blob's* content id — the opaque ref the restore guard
    // resolves against the store. (The blob id content-addresses the chunked
    // object; the canonical bytes are recovered verbatim on read.)
    let canonical_bytes = spork_canon::canonicalize(&original).expect("canonicalize");
    let (conversation_ref, _stats) = store
        .put_blob_bytes(&canonical_bytes)
        .expect("store transcript blob");

    // ---- The node binds the code snapshot AND the conversation ref. ---------
    std::fs::write(
        src_dir.join("src/main.rs"),
        b"fn main() { println!(\"hello\"); }",
    )
    .unwrap();
    std::fs::write(src_dir.join("Cargo.toml"), b"[package]\nname = \"demo\"\n").unwrap();

    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);
    let (snapshot, _root_tree, _stats) = store
        .capture_snapshot(&src_dir, &matcher, profile.hash(), None)
        .expect("capture snapshot");

    let mut graph = GraphService::open_in_memory(log.writer()).unwrap();
    graph.register_builtin_snapshot().unwrap();
    let node = graph
        .create_node(
            BUILTIN_SNAPSHOT_KIND,
            None,
            vec![],
            "main",
            // The conversation ref is the content hash of the bound transcript:
            // code and conversation are bound by content address, not by side
            // table (DESIGN §6.3, §11.4).
            json!({ "origin": "manual", "conversationRef": conversation_ref.to_hex() }),
            true,
            Some(snapshot),
        )
        .unwrap()
        .id;

    // ---- Seam 3 (F3 restore): restore code + conversation atomically. -------
    let guard = RestoreGuard::new(graph, store, workdir.clone(), log.writer());
    let outcome = guard.restore(node).expect("restore");

    // The restore reports both bound refs and an empty external-effects log.
    assert_eq!(outcome.node_id, node);
    assert_eq!(outcome.restored_snapshot, snapshot);
    assert_eq!(
        outcome.restored_conversation,
        Some(conversation_ref),
        "the restore must resolve the bound canonical transcript"
    );
    assert!(outcome.external_effects.is_empty());

    // The code is materialized byte-identically (the §10.1 invariant).
    assert_eq!(read_tree(&workdir), read_tree(&src_dir));

    // ---- The bound conversation restores back into the exact transcript. ----
    // Read the conversation blob back out of the CAS through the same content id
    // the node bound, decode it, and confirm it is the very chat turn that
    // round-tripped through the adapter — code and conversation are restored
    // together, as one bound unit. (A fresh store over the same CAS dir proves
    // the bytes are durably content-addressed, not held in process memory.)
    let store2 = ObjectStore::new(LooseStore::open(&cas_dir).unwrap());
    let recovered = store2.read_blob(&conversation_ref).expect("read blob");
    let recovered_transcript: CanonicalTranscript =
        serde_json::from_slice(&recovered).expect("decode bound transcript");
    assert_eq!(
        recovered_transcript, original,
        "the conversation restored alongside the code is the bound chat turn"
    );
    assert_eq!(
        recovered_transcript.content_hash().expect("content_hash"),
        transcript_hash,
        "the restored conversation content-addresses to its bound identity"
    );

    // HEAD now points at the restored node — the restore is an event.
    guard.with_graph(|g| {
        assert_eq!(g.projection().ref_target("HEAD").unwrap(), Some(node));
    });
}

/// The unknown-node guard rejects a restore whose node id was never created,
/// independent of any bound transcript — keeps the cross-seam path fail-closed.
#[test]
fn restore_of_unknown_node_with_transcript_seam_loaded_is_divergence() {
    let root = TempDir::new().unwrap();
    let cas_dir = root.path().join("cas");
    std::fs::create_dir_all(&cas_dir).unwrap();
    let log = EventLog::open(&root.path().join("log.db")).unwrap();
    let store = ObjectStore::new(LooseStore::open(&cas_dir).unwrap());
    let mut graph = GraphService::open_in_memory(log.writer()).unwrap();
    graph.register_builtin_snapshot().unwrap();

    let guard = RestoreGuard::new(graph, store, root.path().join("work"), log.writer());
    let err = guard.restore(Ulid::new()).unwrap_err();
    assert!(matches!(
        err,
        spork_restore::RestoreError::Divergence { .. }
    ));
}
