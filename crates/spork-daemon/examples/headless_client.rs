//! A headless client that drives the Spork daemon over the real in-process IPC —
//! the F3 headless-core demo, with no GUI (DESIGN.md §5.5, §14.1).
//!
//! Run with: `cargo run -p spork-daemon --example headless_client`.
//!
//! It exercises the full flow the deferred F3-UI renderer will later drive over
//! the same frozen contract:
//! 1. open a daemon and subscribe to the ordered event stream;
//! 2. capture the working tree and create a snapshot node — observe that the
//!    mutation returns only an `op_id` while the node arrives as an event;
//! 3. branch-fork (metadata only) and read it back from the view-model;
//! 4. restore — code + bound conversation move together atomically;
//! 5. print the denormalized `GraphView` the renderer would render.

use spork_daemon::{Command, CommandHandler, CommandResult, Daemon};
use ulid::Ulid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. A daemon rooted in a fresh temp dir, plus an ordered-event subscription.
    let root = tempfile::tempdir()?;
    let daemon = Daemon::open(root.path())?;
    let events = daemon.subscribe_events();
    println!("daemon opened at {:?}", root.path());

    // 2. Put a file in the working tree, capture it, and create a node bound to
    //    a conversation. The mutation returns an op_id; the node arrives via the
    //    event stream.
    std::fs::write(daemon.workdir().join("main.rs"), b"fn main() {}\n")?;
    let conversation = daemon.put_conversation(b"agent: scaffolded main")?;
    let (snapshot_hash, _root_tree) = daemon.capture_working_tree()?;
    let result = daemon.dispatch(Command::NodeCreate {
        kind: "snapshot".into(),
        type_version: "1.0.0".into(),
        parent_ids: vec![],
        branch_id: "main".into(),
        payload: serde_json::json!({
            "origin": "manual",
            "conversationRef": conversation.to_hex(),
        }),
        owns_snapshot: true,
        snapshot_hash: Some(snapshot_hash),
    })?;
    let op_id = result.op_id().expect("a mutation returns an op_id");
    let node_id = node_id_of(&result);
    println!("node.create returned op_id={op_id} (state arrives via events, not here)");
    let event = events.recv()?;
    println!("  ↳ event: {event:?}");

    // 3. Branch-fork (zero bytes copied) and read the ref back from the view.
    daemon.dispatch(Command::BranchFork {
        from_node_id: node_id,
        name: "experiment".into(),
    })?;
    // Drain the fork's two events.
    let _ = events.recv()?;
    let _ = events.recv()?;
    let view = daemon.graph_view();
    println!(
        "branch.fork created ref 'experiment' -> {:?}",
        view.ref_target("experiment")
    );

    // 4. Restore the node: code + conversation atomically; HEAD moves to it.
    daemon.dispatch(Command::NodeRestore { node_id })?;
    let _ = events.recv()?; // RESTORE_PERFORMED
    let _ = events.recv()?; // REF_MOVED (HEAD)
    println!(
        "node.restore moved HEAD -> {:?}",
        daemon.graph_view().ref_target("HEAD")
    );

    // 5. Print the denormalized view-model the renderer would consume.
    let view = daemon.graph_view();
    println!(
        "graph_view: {} node(s), {} edge(s), {} ref(s)",
        view.nodes.len(),
        view.edges.len(),
        view.refs.len()
    );
    println!("{}", serde_json::to_string_pretty(&view)?);

    Ok(())
}

fn node_id_of(result: &CommandResult) -> Ulid {
    match result {
        CommandResult::Mutation { ids, .. } => {
            Ulid::from_string(ids["nodeId"].as_str().expect("nodeId")).expect("valid ULID")
        }
        other => panic!("expected a Mutation result, got {other:?}"),
    }
}
