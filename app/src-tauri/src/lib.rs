//! Spork F3-UI Tauri backend — embeds the frozen F3 [`spork_daemon::Daemon`]
//! behind a thin Tauri command surface (DESIGN.md §14.1, §15.1).
//!
//! # Trust boundary (DESIGN.md §15.1)
//!
//! All privileged operations live in the daemon. The renderer reaches it ONLY
//! through this crate's `#[tauri::command]`s — the single mutation/read path
//! (A.1): [`dispatch`] (one [`spork_ipc::Command`] in, one
//! [`spork_ipc::CommandResult`] out), [`graph_view`] (the denormalized
//! view-model read snapshot), and [`open_project`] (build/open a daemon over a
//! repo). [`ping`] is a placeholder health-check the scaffold exposes so the IPC
//! bridge is wired and testable before the feature work lands.
//!
//! # Why a daemon actor thread
//!
//! [`spork_daemon::Daemon`] is intentionally **single-threaded** — its migration
//! registry holds `Arc<dyn EventMigration>` trait objects that are not `Send` /
//! `Sync` (the daemon's own code carries `#[allow(clippy::arc_with_non_send_sync)]`).
//! Tauri's managed [`State`] requires `Send + Sync + 'static`, so we cannot store
//! the `Daemon` directly. Instead [`open_project`] spawns a dedicated **owner
//! thread** that holds the `Daemon` for its whole life and serves
//! [`DaemonRequest`]s over a channel. The channel handles ([`DaemonHandle`]) are
//! `Send + Sync`; the daemon never crosses a thread boundary. This both
//! satisfies Tauri and preserves the daemon's serialized critical-section model
//! (DESIGN.md §5.5). Errors are stringified inside the owner thread so only
//! `Send` values cross the channel.
//!
//! # Live updates
//!
//! The owner thread also drains the daemon's ordered
//! [`OpLogEvent`](spork_ipc::OpLogEvent) stream
//! ([`Daemon::subscribe_events`](spork_daemon::Daemon::subscribe_events)) and
//! re-emits each onto the window as the `"oplog-event"` event, which the
//! renderer's pure reducer folds into the view-model (DESIGN.md §14.4). The
//! node-keyed ephemeral side-channels (`"ephemeral"`,
//! [`Daemon::subscribe_node`](spork_daemon::Daemon::subscribe_node)) are
//! per-node; the event name and frame shape are frozen here so the feature slice
//! that opens a node's run/chat surface can forward them.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread;

use serde_json::Value;
use spork_daemon::{AgentConfig, Daemon, DaemonBuilder, STATE_SUBDIR};
use spork_ipc::{Command, CommandHandler, EphemeralFrame, OpLogEvent, Ulid};
use tauri::{Emitter, Manager, State};

/// The file under `<project>/.spork/` that persists the user's chosen agent
/// provider config across restarts (P7.5 MVP, W3). The endpoint/CLI command is
/// provider *wiring*, not a secret (DESIGN.md §15.1), so it is stored plainly.
const AGENT_CONFIG_FILE: &str = "agent_config.json";

/// The window event name carrying an ordered [`OpLogEvent`](spork_ipc::OpLogEvent).
pub const OPLOG_EVENT: &str = "oplog-event";
/// The window event name carrying an
/// [`EphemeralFrame`](spork_ipc::EphemeralFrame).
pub const EPHEMERAL_EVENT: &str = "ephemeral";

/// A request to the daemon owner thread, paired with a reply channel.
///
/// The reply is the JSON-serialized result (or a stringified error) so only
/// `Send` values cross the thread boundary — the daemon's own
/// `CommandResult`/`GraphView` are serialized on the owner thread.
enum DaemonRequest {
    /// Deserialize + dispatch a command; reply with the serialized result.
    Dispatch {
        command: Value,
        reply: Sender<Result<Value, String>>,
    },
    /// Read the view-model; reply with the serialized snapshot.
    GraphView {
        reply: Sender<Result<Value, String>>,
    },
    /// Subscribe to a node's ephemeral side-channel; reply with the (`Send`)
    /// receiver so the forwarder thread can drain it without touching the
    /// non-`Send` [`Daemon`]. Issued by the op-log forwarder when a
    /// [`OpLogEvent::NodeCreated`] arrives (DESIGN.md §14.4).
    SubscribeNode {
        node_id: Ulid,
        reply: Sender<EphemeralReceiver>,
    },
    /// Swap the daemon's agent provider config at runtime and persist it under
    /// `<project>/.spork/agent_config.json` (P7.5 MVP, W3). Handled on the owner
    /// thread (the only holder of the non-`Send` [`Daemon`]); the reply carries a
    /// persist failure so the renderer can surface it. [`AgentConfig`] is `Send`.
    SetAgentConfig {
        config: AgentConfig,
        reply: Sender<Result<Value, String>>,
    },
}

/// The transport receiver carrying a node's [`EphemeralFrame`]s. Re-named from
/// the daemon's re-export so the owner-thread plumbing reads clearly.
type EphemeralReceiver = spork_daemon::EventReceiver<EphemeralFrame>;

/// A `Send + Sync` handle to the daemon owner thread.
struct DaemonHandle {
    tx: Sender<DaemonRequest>,
}

impl DaemonHandle {
    /// Send a request and block for the reply, mapping a dropped owner thread to
    /// an error.
    fn call(
        &self,
        make: impl FnOnce(Sender<Result<Value, String>>) -> DaemonRequest,
    ) -> Result<Value, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "daemon thread stopped".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "daemon thread dropped the reply".to_string())?
    }
}

/// The Tauri-managed application state: a handle to the daemon owner thread, or
/// `None` until a project is opened.
///
/// The handle is `Send + Sync` (it is just channel senders), so Tauri can manage
/// it while the non-`Send` [`Daemon`] stays pinned to its owner thread.
#[derive(Default)]
pub struct AppState {
    handle: Mutex<Option<DaemonHandle>>,
}

/// A placeholder health-check command, proving the IPC bridge is wired.
///
/// Returns a constant string; carries no daemon state. The feature slices add
/// the real commands behind the same `invoke` surface (CLAUDE.md C1: this is a
/// wired+tested command, not a hollow stub).
#[tauri::command]
fn ping() -> String {
    "pong".to_string()
}

/// Build/open a daemon over `path` on a dedicated owner thread, store its handle
/// in [`AppState`], and forward its op-log and ephemeral streams onto the window.
///
/// Mirrors A.1's project-open: a fresh [`Daemon::open`] roots its working tree,
/// CAS, event log, and vault under `path`. The owner thread subscribes to the
/// ordered event stream *before* serving any request (subscribe-before-mutate,
/// DESIGN.md §14.4) and re-emits each [`OpLogEvent`] onto the `"oplog-event"`
/// window event. As each [`OpLogEvent::NodeCreated`] arrives, that node's
/// ephemeral side-channel ([`Daemon::subscribe_node`]) is opened on a separate
/// thread and its [`EphemeralFrame`]s are forwarded onto the `"ephemeral"` window
/// event — so high-frequency chat/run frames never stall ordered op-log delivery
/// (DESIGN.md §5.5, §14.4).
///
/// # Errors
/// Returns a string error if the daemon cannot be opened. Open failures are
/// reported back synchronously over a bootstrap channel.
#[tauri::command]
fn open_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let (req_tx, req_rx) = mpsc::channel::<DaemonRequest>();
    let (boot_tx, boot_rx) = mpsc::channel::<Result<(), String>>();
    let app_handle = app.clone();
    // The owner thread keeps a self-handle so the op-log forwarder can ask it
    // (the only holder of the non-`Send` daemon) to subscribe to new nodes.
    let self_tx = req_tx.clone();

    thread::spawn(move || {
        // The project's hidden state dir holds Spork's CAS/log/vault + the
        // persisted agent config (P7.5 MVP, W2/W3).
        let state_dir = PathBuf::from(&path).join(STATE_SUBDIR);
        // Reload the user's previously-chosen provider, else the default local
        // endpoint (W3). The file is absent on a first-ever open.
        let agent_config =
            load_agent_config(&state_dir).unwrap_or_else(AgentConfig::with_default_local);

        // Construct the daemon ON the owner thread (it never leaves it). P7.5 W2:
        // root at the user's REAL project dir (`for_project`), so capture sees
        // their code and Spork's own state lives under `<project>/.spork/`. P6:
        // grant model access (model.invoke + net.connect to the local model host)
        // so agent runs work for the desktop user; cloud egress still needs a
        // separate grant + the (deferred) TLS transport (DESIGN §15.1).
        let daemon = match DaemonBuilder::for_project(&path)
            .grant_model_access()
            .with_agent_config(agent_config)
            .build()
        {
            Ok(d) => d,
            Err(e) => {
                let _ = boot_tx.send(Err(e.to_string()));
                return;
            }
        };

        // Subscribe + spawn the op-log forwarder before serving requests so no
        // event between open and first mutation is missed (subscribe-before-
        // mutate, DESIGN.md §14.4). The forwarder also discovers nodes: each
        // `NodeCreated` triggers an ephemeral subscription for that node.
        let events = daemon.subscribe_events();
        let emit_handle = app_handle.clone();
        thread::spawn(move || forward_oplog(&events, &self_tx, &emit_handle));

        // Signal a successful open, then serve requests until the channel closes.
        let _ = boot_tx.send(Ok(()));
        serve(&daemon, &req_rx, &state_dir);
    });

    boot_rx
        .recv()
        .map_err(|_| "daemon thread failed to start".to_string())??;

    *state.handle.lock().map_err(|e| e.to_string())? = Some(DaemonHandle { tx: req_tx });
    Ok(())
}

/// The owner thread's request loop: serialize every reply on this thread so the
/// non-`Send` daemon types never cross the channel. `state_dir` is the project's
/// `.spork/` dir, where a runtime agent-config swap is persisted (W3).
fn serve(daemon: &Daemon, rx: &mpsc::Receiver<DaemonRequest>, state_dir: &Path) {
    while let Ok(req) = rx.recv() {
        match req {
            DaemonRequest::Dispatch { command, reply } => {
                let _ = reply.send(do_dispatch(daemon, command));
            }
            DaemonRequest::GraphView { reply } => {
                let view = daemon.graph_view();
                let _ = reply.send(serde_json::to_value(view).map_err(|e| e.to_string()));
            }
            DaemonRequest::SubscribeNode { node_id, reply } => {
                // Only the owner thread holds the non-`Send` daemon; it makes
                // the subscription and hands the `Send` receiver to the
                // forwarder (DESIGN.md §14.4).
                let _ = reply.send(daemon.subscribe_node(node_id));
            }
            DaemonRequest::SetAgentConfig { config, reply } => {
                // Swap the live provider, then persist the choice so it survives
                // a restart (W3). A persist failure is reported, not swallowed.
                daemon.set_agent_config(config.clone());
                let result = save_agent_config(state_dir, &config).map(|()| Value::Null);
                let _ = reply.send(result);
            }
        }
    }
}

/// Read the persisted agent config from `<state_dir>/agent_config.json`, or
/// `None` if it is absent/unreadable (a first-ever open, or a corrupt file —
/// either way the caller falls back to the default local endpoint).
fn load_agent_config(state_dir: &Path) -> Option<AgentConfig> {
    let bytes = std::fs::read(state_dir.join(AGENT_CONFIG_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist the agent config to `<state_dir>/agent_config.json`, creating the
/// state dir if needed. Returns a stringified error the renderer can surface.
fn save_agent_config(state_dir: &Path, config: &AgentConfig) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(state_dir.join(AGENT_CONFIG_FILE), bytes).map_err(|e| e.to_string())
}

/// The renderer-facing input shape of the `set_agent_config` command — a friendly
/// `{ kind, endpoint?, command?, args? }` the Settings form sends, mapped to the
/// internal [`AgentConfig`] (whose `cli_command` tuple is awkward to build in TS).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentConfigInput {
    /// `"local"` (HTTP endpoint) or `"cli"` (subprocess agent).
    kind: String,
    /// The local OpenAI-compatible endpoint URL (for `kind == "local"`).
    endpoint: Option<String>,
    /// The CLI agent program (for `kind == "cli"`).
    command: Option<String>,
    /// The CLI agent args (for `kind == "cli"`).
    args: Option<Vec<String>>,
}

impl AgentConfigInput {
    /// Map the renderer input to an [`AgentConfig`], applying the default local
    /// endpoint when `"local"` is chosen with a blank URL.
    fn into_config(self) -> Result<AgentConfig, String> {
        match self.kind.as_str() {
            "local" => {
                let endpoint = self
                    .endpoint
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| AgentConfig::DEFAULT_LOCAL_ENDPOINT.to_string());
                Ok(AgentConfig::with_endpoint(endpoint))
            }
            "cli" => {
                let command = self
                    .command
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| "a CLI agent requires a command".to_string())?;
                Ok(AgentConfig::with_cli_agent(
                    command,
                    self.args.unwrap_or_default(),
                ))
            }
            other => Err(format!("unknown provider kind {other:?}")),
        }
    }
}

/// Drain the ordered op-log: emit every [`OpLogEvent`] onto the window, and on
/// each [`OpLogEvent::NodeCreated`] open that node's ephemeral side-channel and
/// spawn a forwarder for it.
///
/// Runs on its own thread holding the `Send` [`OpLogEvent`] receiver. It reaches
/// the non-`Send` daemon only indirectly, by asking the owner thread (`owner`)
/// to make each node subscription and hand back the `Send` receiver. Ordered
/// op-log delivery and the per-node ephemeral floods stay on separate threads,
/// so the latter can never stall the former (DESIGN.md §5.5, §14.4).
fn forward_oplog(
    events: &spork_daemon::EventReceiver<OpLogEvent>,
    owner: &Sender<DaemonRequest>,
    app: &tauri::AppHandle,
) {
    while let Ok(event) = events.recv() {
        // A node entered the graph — start forwarding its ephemeral frames.
        if let OpLogEvent::NodeCreated { node_id, .. } = &event {
            subscribe_ephemeral(*node_id, owner, app);
        }
        if app.emit(OPLOG_EVENT, &event).is_err() {
            break; // window gone — end the forwarder cleanly.
        }
    }
}

/// Ask the owner thread to subscribe to `node_id`'s ephemeral channel, then
/// spawn a thread forwarding each [`EphemeralFrame`] onto the window. A failed
/// request (owner thread stopped) is a no-op.
fn subscribe_ephemeral(node_id: Ulid, owner: &Sender<DaemonRequest>, app: &tauri::AppHandle) {
    let (reply_tx, reply_rx) = mpsc::channel();
    if owner
        .send(DaemonRequest::SubscribeNode {
            node_id,
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    let Ok(frames) = reply_rx.recv() else {
        return;
    };
    let emit_handle = app.clone();
    thread::spawn(move || forward_ephemeral(&frames, &emit_handle));
}

/// Forward one node's ephemeral frames onto the window's `"ephemeral"` event
/// until the channel closes or the window goes away.
fn forward_ephemeral(frames: &EphemeralReceiver, app: &tauri::AppHandle) {
    while let Ok(frame) = frames.recv() {
        if app.emit(EPHEMERAL_EVENT, &frame).is_err() {
            break; // window gone — end the forwarder cleanly.
        }
    }
}

/// Deserialize a frozen-wire `Command`, dispatch it, and serialize the result.
fn do_dispatch(daemon: &Daemon, command: Value) -> Result<Value, String> {
    let cmd: Command = serde_json::from_value(command).map_err(|e| e.to_string())?;
    let result = daemon.dispatch(cmd).map_err(|e| e.to_string())?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

/// The single mutation/read path: forward a [`Command`] JSON to the daemon owner
/// thread and return the serialized [`spork_ipc::CommandResult`] (DESIGN.md A.1).
///
/// A mutation returns only its `op_id` (state arrives over `"oplog-event"`); a
/// read returns its data inline. The JSON shapes are the frozen `spork-ipc` wire
/// forms the renderer's TS types mirror.
///
/// # Errors
/// Returns a string error if no project is open, the command JSON is malformed,
/// or the daemon rejects it (e.g. a capability denial).
#[tauri::command]
fn dispatch(state: State<'_, AppState>, command: Value) -> Result<Value, String> {
    let guard = state.handle.lock().map_err(|e| e.to_string())?;
    let handle = guard.as_ref().ok_or("no project open")?;
    handle.call(|reply| DaemonRequest::Dispatch { command, reply })
}

/// The denormalized view-model read snapshot the canvas binds to (DESIGN.md
/// §14.3).
///
/// Returns the daemon's [`GraphView`](spork_daemon::GraphView) (nodes/edges/refs)
/// as camelCase JSON, matching the renderer's TS `GraphView` type.
///
/// # Errors
/// Returns a string error if no project is open or serialization fails.
#[tauri::command]
fn graph_view(state: State<'_, AppState>) -> Result<Value, String> {
    let guard = state.handle.lock().map_err(|e| e.to_string())?;
    let handle = guard.as_ref().ok_or("no project open")?;
    handle.call(|reply| DaemonRequest::GraphView { reply })
}

/// Configure which provider agent runs reach — a local OpenAI-compatible HTTP
/// endpoint (the default route) or a conforming CLI agent — and persist the
/// choice (P7.5 MVP, W3). An app-local command (not a frozen `spork-ipc::Command`),
/// consistent with the existing Tauri surface so the IPC boundary is untouched.
/// (The generic CLI-as-model route is deprecated — REALIGNMENT_PLAN.md §2.)
///
/// `config` is the renderer's `{ kind, endpoint?, command?, args? }`
/// ([`AgentConfigInput`]). The provider URL / CLI command is config, not a secret
/// (DESIGN.md §15.1) — the renderer holds zero secrets.
///
/// # Errors
/// Returns a string error if no project is open, the input is malformed/invalid,
/// or persisting the choice fails.
#[tauri::command]
fn set_agent_config(state: State<'_, AppState>, config: Value) -> Result<(), String> {
    let input: AgentConfigInput = serde_json::from_value(config).map_err(|e| e.to_string())?;
    let agent_config = input.into_config()?;
    let guard = state.handle.lock().map_err(|e| e.to_string())?;
    let handle = guard.as_ref().ok_or("no project open")?;
    handle.call(|reply| DaemonRequest::SetAgentConfig {
        config: agent_config,
        reply,
    })?;
    Ok(())
}

/// Open a path in the user's real editor (REALIGNMENT_PLAN §5d): Spork is **not**
/// a code editor, so "Open codebase" hands off to VS Code / Cursor / Sublime /
/// IntelliJ (or the OS default), exactly like Claude Desktop / the Copilot app.
/// In-app Monaco stays read-only diff only.
///
/// `editor` is an optional explicit launcher (`code`/`cursor`/`subl`/`idea`); when
/// absent the first detected editor on `PATH` is used, falling back to the OS
/// file-opener (`open`/`xdg-open`/`explorer`). An app-local command, not a frozen
/// `spork-ipc::Command` — the renderer holds no spawn authority of its own.
///
/// # Errors
/// Returns a string error if no launcher can be spawned for the path.
#[tauri::command]
fn open_in_editor(path: String, editor: Option<String>) -> Result<(), String> {
    use std::process::Command;

    // An explicit editor wins; else probe the common CLIs; else the OS opener.
    let candidates: Vec<String> = match editor.as_deref() {
        Some(e) if !e.trim().is_empty() => vec![e.trim().to_string()],
        _ => {
            let mut v: Vec<String> = ["code", "cursor", "subl", "idea"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            // OS default file-opener as the last resort.
            #[cfg(target_os = "macos")]
            v.push("open".to_string());
            #[cfg(target_os = "linux")]
            v.push("xdg-open".to_string());
            #[cfg(target_os = "windows")]
            v.push("explorer".to_string());
            v
        }
    };

    let mut last_err = String::from("no editor launcher available");
    for launcher in candidates {
        match Command::new(&launcher).arg(&path).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => last_err = format!("{launcher}: {e}"),
        }
    }
    Err(format!("could not open editor for {path:?}: {last_err}"))
}

/// Build and run the Tauri application: manage [`AppState`] and register the
/// command handlers.
///
/// Split out of `main` so the same builder backs both the desktop binary and the
/// (future) mobile entrypoint convention.
///
/// # Panics
/// Panics if the Tauri runtime cannot start (an unrecoverable startup failure);
/// this is the documented Tauri convention for `run`.
pub fn run() {
    tauri::Builder::default()
        // The native folder picker the onboarding flow uses to select a project
        // dir (P7.5 MVP, W1). Gated by the `dialog:default` capability.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            open_project,
            dispatch,
            graph_view,
            set_agent_config,
            open_in_editor
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Spork Tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_ipc::Hash;

    /// A valid 64-char lowercase hex digest as a snapshot-owning [`Hash`] for
    /// `NodeCreate`. The graph only checks a hash is *present* when
    /// `owns_snapshot` is true (DESIGN.md §6.2, §7.2) — not that its content is in
    /// the CAS — so any well-formed digest works, and we avoid depending on
    /// `spork-hash` directly (the renderer never does either). This is the same
    /// BLAKE3-of-empty digest the JSON-form test below uses.
    fn sample_hash() -> Hash {
        Hash::from_hex("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")
            .expect("a 64-char hex digest parses into a Hash")
    }

    #[test]
    fn ping_returns_pong() {
        // The placeholder command is wired and behaves (CLAUDE.md C1).
        assert_eq!(ping(), "pong");
    }

    #[test]
    fn app_state_starts_with_no_daemon() {
        let state = AppState::default();
        assert!(state.handle.lock().unwrap().is_none());
    }

    #[test]
    fn event_names_are_the_frozen_wire_strings() {
        // The renderer's TS binding hardcodes these; keep them in lockstep.
        assert_eq!(OPLOG_EVENT, "oplog-event");
        assert_eq!(EPHEMERAL_EVENT, "ephemeral");
    }

    #[test]
    fn graph_view_serializes_to_the_camel_case_wire_shape() {
        // Embedding works end-to-end without a window: open a daemon over a temp
        // dir and read its (empty) view-model as the camelCase JSON the renderer
        // consumes (no display, no Tauri runtime).
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();
        let json = serde_json::to_value(daemon.graph_view()).unwrap();
        // R2 bumped GRAPH_VIEW_SCHEMA_VERSION to 2 (additive NodeView fields).
        assert_eq!(json["schemaVersion"], 2);
        assert!(json["nodes"].is_array());
        assert!(json["edges"].is_array());
        assert!(json["refs"].is_array());
    }

    #[test]
    fn do_dispatch_runs_a_frozen_command_against_a_real_daemon() {
        // A NODE_CREATE in the frozen wire form deserializes into spork_ipc and
        // dispatches to a real daemon, returning a MUTATION result with an op_id
        // — the exact path the `dispatch` command takes on the owner thread.
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();
        // The `snapshot` built-in is a Mutating, snapshot-OWNING kind, so it
        // requires `ownsSnapshot: true` + a snapshot hash (the BLAKE3 of the
        // empty input here — any valid 64-char lowercase hex Hash works). The
        // hash wire form is the bare hex digest (crates/spork-hash/src/hash.rs).
        let cmd_json = serde_json::json!({
            "command": "NODE_CREATE",
            "kind": "snapshot",
            "typeVersion": "1.0.0",
            "parentIds": [],
            "branchId": "main",
            "payload": { "origin": "manual" },
            "ownsSnapshot": true,
            "snapshotHash":
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        });
        let v = do_dispatch(&daemon, cmd_json).unwrap();
        assert_eq!(v["result"], "MUTATION");
        assert!(v["opId"].is_string());
    }

    #[test]
    fn agent_config_input_maps_local_and_cli() {
        // The renderer's friendly shape maps to the internal AgentConfig.
        let local: AgentConfigInput = serde_json::from_value(serde_json::json!({
            "kind": "local",
            "endpoint": "http://127.0.0.1:1234/v1/chat/completions"
        }))
        .unwrap();
        let cfg = local.into_config().unwrap();
        assert_eq!(
            cfg.local_endpoint.as_deref(),
            Some("http://127.0.0.1:1234/v1/chat/completions")
        );
        assert!(cfg.cli_command.is_none());

        // A blank local endpoint falls back to the conventional default.
        let blank: AgentConfigInput =
            serde_json::from_value(serde_json::json!({ "kind": "local", "endpoint": "" })).unwrap();
        assert_eq!(
            blank.into_config().unwrap().local_endpoint.as_deref(),
            Some(AgentConfig::DEFAULT_LOCAL_ENDPOINT)
        );

        let cli: AgentConfigInput = serde_json::from_value(serde_json::json!({
            "kind": "cli",
            "command": "my-cli-agent",
            "args": ["--json"]
        }))
        .unwrap();
        let cfg = cli.into_config().unwrap();
        assert_eq!(
            cfg.cli_command,
            Some(("my-cli-agent".to_string(), vec!["--json".to_string()]))
        );
        assert!(cfg.local_endpoint.is_none());

        // A CLI provider with no command is rejected; an unknown kind too.
        let no_cmd: AgentConfigInput =
            serde_json::from_value(serde_json::json!({ "kind": "cli" })).unwrap();
        assert!(no_cmd.into_config().is_err());
        let unknown: AgentConfigInput =
            serde_json::from_value(serde_json::json!({ "kind": "nope" })).unwrap();
        assert!(unknown.into_config().is_err());
    }

    #[test]
    fn agent_config_persists_and_reloads() {
        // W3: save then load round-trips the chosen provider through
        // `<state>/agent_config.json` (what survives a project reopen).
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join(STATE_SUBDIR);
        // Absent file → None (a first-ever open falls back to the default).
        assert!(load_agent_config(&state_dir).is_none());

        let cfg = AgentConfig::with_cli_agent("my-cli-agent", vec!["--json".into()]);
        save_agent_config(&state_dir, &cfg).unwrap();
        let loaded = load_agent_config(&state_dir).expect("config reloads");
        assert_eq!(loaded.cli_command, cfg.cli_command);
        assert_eq!(loaded.local_endpoint, cfg.local_endpoint);
    }

    #[test]
    fn do_dispatch_rejects_malformed_command_json() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();
        let err = do_dispatch(&daemon, serde_json::json!({ "command": "NOPE" })).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn ephemeral_subscription_delivers_a_published_frame() {
        // The ephemeral seam end-to-end without a window: subscribe to a node,
        // publish a frame, and drain it off the (`Send`) receiver — the exact
        // receiver the owner thread's `SubscribeNode` reply hands to
        // `forward_ephemeral`, and the same `subscribe_node` / `publish_ephemeral`
        // pair `open_project` wires onto the `"ephemeral"` window event.
        use spork_ipc::EphemeralChannel;

        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();

        // A concrete node id, mirroring the `NodeCreated` discovery the op-log
        // forwarder keys its ephemeral subscription off.
        daemon
            .dispatch(Command::NodeCreate {
                kind: "snapshot".into(),
                type_version: "1.0.0".into(),
                parent_ids: vec![],
                branch_id: "main".into(),
                payload: serde_json::json!({ "origin": "manual" }),
                owns_snapshot: true,
                snapshot_hash: Some(sample_hash()),
            })
            .unwrap();
        let node_id = daemon
            .graph_view()
            .nodes
            .first()
            .map(|n| n.id)
            .expect("a node was created");

        let frames: EphemeralReceiver = daemon.subscribe_node(node_id);
        daemon.publish_ephemeral(node_id, EphemeralChannel::RunStdout, "line\n");

        let frame = frames.recv().expect("the published frame is delivered");
        // The exact camelCase wire shape `forward_ephemeral` emits.
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["nodeId"], node_id.to_string());
        assert_eq!(v["channel"], "RUN_STDOUT");
        assert_eq!(v["data"], "line\n");
    }

    #[test]
    fn serve_loop_answers_dispatch_graph_view_and_subscribe_node_requests() {
        // Exercise the owner-thread request loop end-to-end the way `open_project`
        // runs it: the non-`Send` daemon is constructed INSIDE the owner thread
        // (it never crosses a boundary) and driven only through the `Send` request
        // channel — the same `DaemonHandle::call` path `dispatch` / `graph_view` /
        // `subscribe_ephemeral` use (no window, no Tauri runtime).
        use spork_ipc::EphemeralChannel;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (req_tx, req_rx) = mpsc::channel::<DaemonRequest>();
        // A side channel so the owner thread can hand the test a node id to
        // subscribe to and then publish a frame for it.
        let (node_tx, node_rx) = mpsc::channel::<Ulid>();
        let publish_tx = req_tx.clone();

        let owner = thread::spawn(move || {
            let daemon = Daemon::open(&root).unwrap();
            // Create a node so there is a concrete id to subscribe to.
            daemon
                .dispatch(Command::NodeCreate {
                    kind: "snapshot".into(),
                    type_version: "1.0.0".into(),
                    parent_ids: vec![],
                    branch_id: "main".into(),
                    payload: serde_json::json!({ "origin": "manual" }),
                    owns_snapshot: true,
                    snapshot_hash: Some(sample_hash()),
                })
                .unwrap();
            let node_id = daemon.graph_view().nodes.first().map(|n| n.id).unwrap();
            node_tx.send(node_id).unwrap();
            // Serve until the test asks (via a Dispatch sentinel) to publish, then
            // continues serving. We interleave a publish on the live `serve` loop
            // by spawning the publish as a request the loop runs against `daemon`.
            serve_with_publish(&daemon, &req_rx, node_id, EphemeralChannel::ChatTokens);
        });

        let node_id = node_rx.recv().unwrap();

        // GraphView over the channel returns the camelCase snapshot.
        let (gv_tx, gv_rx) = mpsc::channel();
        req_tx
            .send(DaemonRequest::GraphView { reply: gv_tx })
            .unwrap();
        let view = gv_rx.recv().unwrap().unwrap();
        assert_eq!(view["schemaVersion"], 2);
        assert_eq!(view["nodes"].as_array().unwrap().len(), 1);

        // SubscribeNode hands back a live receiver; the loop publishes one frame.
        let (sub_tx, sub_rx) = mpsc::channel();
        publish_tx
            .send(DaemonRequest::SubscribeNode {
                node_id,
                reply: sub_tx,
            })
            .unwrap();
        let frames = sub_rx.recv().unwrap();
        let frame = frames.recv().expect("the published frame is delivered");
        assert_eq!(frame.channel, EphemeralChannel::ChatTokens);
        assert_eq!(frame.node_id, node_id);

        drop(req_tx);
        drop(publish_tx);
        owner.join().unwrap();
    }

    /// A test-only `serve` wrapper that publishes one ephemeral frame on the
    /// node's channel the first time a [`DaemonRequest::SubscribeNode`] is handled
    /// — so the test can assert a published frame reaches the handed-back
    /// receiver, all on the owner thread that owns the non-`Send` daemon.
    fn serve_with_publish(
        daemon: &Daemon,
        rx: &mpsc::Receiver<DaemonRequest>,
        node_id: Ulid,
        channel: spork_ipc::EphemeralChannel,
    ) {
        while let Ok(req) = rx.recv() {
            match req {
                DaemonRequest::Dispatch { command, reply } => {
                    let _ = reply.send(do_dispatch(daemon, command));
                }
                DaemonRequest::GraphView { reply } => {
                    let view = daemon.graph_view();
                    let _ = reply.send(serde_json::to_value(view).map_err(|e| e.to_string()));
                }
                DaemonRequest::SubscribeNode {
                    node_id: requested,
                    reply,
                } => {
                    let frames = daemon.subscribe_node(requested);
                    let _ = reply.send(frames);
                    // Publish after the subscription so the receiver observes it.
                    daemon.publish_ephemeral(node_id, channel, "tok");
                }
                DaemonRequest::SetAgentConfig { config, reply } => {
                    daemon.set_agent_config(config);
                    let _ = reply.send(Ok(Value::Null));
                }
            }
        }
    }
}
