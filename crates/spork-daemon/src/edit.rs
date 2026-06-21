//! P7.5 MVP dispatch: `node.agentEdit` — the **trusted-local code-changing edit
//! loop** (docs/MVP_PLAN.md W4; DESIGN.md §6.6, §8.2, §9.2, §11.1, §15.3).
//!
//! This is the headline MVP capability: ask the agent to change the code and get
//! a real Edit node. It runs **entirely on the shipped F4 `WorktreeCow` tier**
//! ("right for trusted edits", `spork-exec`): the daemon provisions a
//! content-addressed **CoW copy** of the target's snapshot — the user's real
//! checkout is never touched (DESIGN.md §9.2, §15.3) — runs a bounded agent
//! tool-loop against that copy, captures the mutated tree into a new snapshot, and
//! creates a `codebase-edit` node owning it. Only *untrusted-marketplace* executor
//! isolation (microVM/WASM) is P8; this is wiring over frozen seams (CLAUDE.md
//! C2/C3): a new dispatch arm + a new consumer of the
//! [`IsolationBackend`](spork_exec::IsolationBackend) /
//! [`run_turn`](spork_agent::run_turn) seams, no contract edited.
//!
//! # The loop
//!
//! 1. provision a CoW [`Workspace`] over the target's snapshot (TTL lease);
//! 2. iterate (bounded by [`MAX_ITERATIONS`] / [`MAX_COST_MICROS`]): call
//!    [`run_turn`](spork_agent::run_turn), execute each
//!    [`ContentBlock::ToolCall`](spork_provider::ContentBlock) against the
//!    workspace (a small **trusted tool set** — read/write/list/run-command/
//!    apply-patch), append the [`ToolResult`](spork_provider::ContentBlock) to the
//!    transcript, and continue until the model stops emitting tool calls;
//! 3. `capture_path` the mutated tree → wrap as a snapshot;
//! 4. create the Edit node owning it, parented on the target, binding the
//!    transcript as `conversation_ref`;
//! 5. apply §6.6 **fork-on-divergence** (a tip continues its branch; a non-tip
//!    auto-forks a new branch so a line is never silently overwritten);
//! 6. auto-run a change-scoped **Sanity** check (DESIGN.md §8.2);
//! 7. tear down the lease.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use spork_asset::LocalCasAssetStore;
use spork_broker::{Capability, RequestedScope};
use spork_cas::{LooseStore, ObjectStore};
use spork_exec::{
    CancelToken, EnvManifest, IsolationBackend, LeaseLedger, PreparedRun, Workspace,
    WorktreeCowBackend,
};
use spork_graph::EdgeType;
use spork_ipc::{CommandResult, OpLogEvent};
use spork_nodes::{EditPayload, ToolCall as EditToolCall, EDIT_KIND};
use spork_provider::{CanonicalTranscript, CanonicalTurn, ContentBlock, ModelSelector, Role};
use ulid::Ulid;

use crate::agent::{map_agent_error_for, parse_privacy, DaemonTransports};
use crate::core::{Daemon, WORKTREE_GLOB};
use crate::error::DaemonError;

/// Max agent tool-loop iterations per edit (the user-chosen W4 ceiling). Bounds a
/// runaway loop that never stops emitting tool calls.
const MAX_ITERATIONS: u32 = 20;
/// Max accumulated model spend per edit, in micro-USD ($5.00, the W4 ceiling).
const MAX_COST_MICROS: u64 = 5_000_000;
/// The per-run model budget the dispatch *requests* from the broker (the granted
/// budget is the real ceiling), matching the read-only run.
const PER_RUN_TOKEN_CAP: u64 = 1_000_000;
/// The per-run USD cap requested, micro-USD ($10).
const PER_RUN_USD_MICROS: u64 = 10_000_000;
/// The semver stamped on the built-in Edit node.
const EDIT_TYPE_VERSION: &str = "1.0.0";

/// The system prompt that tells a (real) model the trusted tool protocol. A
/// scripted test agent ignores it; a capable local/CLI model uses it to drive the
/// loop. The wire shape mirrors the canonical JSONL tool blocks.
const TOOL_SYSTEM_PROMPT: &str = "You are a coding agent editing a copy of a repository. \
Use tool calls to inspect and modify files, then reply with a short summary when done. \
Available tools (emit a tool_call with the given name and JSON input): \
read_file{path}, write_file{path,content}, list_dir{path}, run_command{command,args?}, \
apply_patch{path,patch} (patch is a unified diff). Paths are relative to the repo root.";

/// The outcome of running the bounded edit tool-loop.
struct EditLoopOutcome {
    /// The full accumulated transcript (system + user + assistant + tool turns).
    transcript: CanonicalTranscript,
    /// The repo-relative paths the agent wrote/patched (the change scope).
    files_changed: Vec<String>,
    /// The tool calls the agent made, as `(name, summary)` for the Edit payload.
    tool_calls: Vec<(String, String)>,
    /// Accumulated model spend across the loop, micro-USD.
    cost_micros: u64,
    /// The resolved provider/model label (`provider/model`).
    model_label: String,
    /// The model's final free-text answer (the last text block seen).
    answer: String,
}

impl Daemon {
    /// `node.agentEdit`: run the trusted edit loop and create an Edit node
    /// (DESIGN.md §6.6, §9.2). See the module docs for the full flow.
    pub(crate) fn cmd_node_agent_edit(
        &self,
        target_node_id: Ulid,
        prompt: &str,
        model_key: &str,
        privacy_str: &str,
    ) -> Result<CommandResult, DaemonError> {
        // Resolve the target (it must own a snapshot to edit a copy of).
        let (branch_id, parent_snapshot) = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .get_node(target_node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .ok_or_else(|| DaemonError::NotFound(format!("node {target_node_id}")))?;
            let snapshot = env.snapshot_hash.ok_or_else(|| {
                DaemonError::NotFound(format!("node {target_node_id} owns no snapshot"))
            })?;
            (env.branch_id, snapshot)
        };
        // §6.6 decision is taken BEFORE we add the edit child (which would itself
        // make the target a non-tip): a tip continues its branch, a non-tip forks.
        let is_tip = self.node_is_tip(target_node_id)?;

        // Snapshot the agent config once (it may be swapped at runtime), then
        // authorize: the model spend, the snapshot write (capture), process spawn
        // (run_command + the worktree backend), and the local host if configured.
        let agent_config = self
            .agent_config
            .lock()
            .expect("agent config mutex poisoned")
            .clone();
        self.authorize(
            Capability::ModelInvoke,
            &RequestedScope::model(PER_RUN_TOKEN_CAP, PER_RUN_USD_MICROS),
        )?;
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        self.authorize(
            Capability::ProcessSpawn,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        if let Some(host) = agent_config.local_host() {
            self.authorize(Capability::NetConnect, &RequestedScope::host(host))?;
        }

        let privacy = parse_privacy(privacy_str)?;
        let selector = if model_key.is_empty() {
            ModelSelector::default_model()
        } else {
            ModelSelector::pinned(model_key)
        };

        // Build the CoW backend over the SAME on-disk CAS the graph references (a
        // second handle to the loose store — the sanctioned reopen pattern). The
        // captured tree therefore lands in the store the daemon reads from.
        let backend = self.build_edit_backend()?;
        let ws = backend
            .provision(parent_snapshot, &EnvManifest::default())
            .map_err(|e| DaemonError::Io(format!("provision edit workspace: {e}")))?;

        // Run the bounded loop against the CoW copy (no core lock held — these are
        // the slow, networked, command-running parts).
        let outcome =
            match self.run_edit_loop(&backend, &ws, &agent_config, &selector, privacy, prompt) {
                Ok(o) => o,
                Err(e) => {
                    // Best-effort teardown on failure so the lease/worktree is released.
                    let _ = backend.teardown(ws);
                    return Err(e);
                }
            };

        // Capture the mutated tree, then release the workspace.
        let captured_tree = backend
            .capture_path(&ws, ws.root.as_path())
            .map_err(|e| DaemonError::Io(format!("capture edit workspace: {e}")))?;
        backend
            .teardown(ws)
            .map_err(|e| DaemonError::Io(format!("teardown edit workspace: {e}")))?;

        // Wrap the captured tree in a snapshot object (in the shared CAS) and bind
        // the transcript as the node's conversation_ref.
        let new_snapshot = self.put_snapshot_over(captured_tree)?;
        let transcript_bytes = serde_json::to_vec(&outcome.transcript)
            .map_err(|e| DaemonError::Agent(format!("encode edit transcript: {e}")))?;
        let conversation_ref = self.put_conversation(&transcript_bytes)?;

        // Build the versioned Edit payload (DESIGN.md §13.4 code+conversation).
        let diff_summary = if outcome.answer.trim().is_empty() {
            format!(
                "agent edit: {} file(s) changed",
                outcome.files_changed.len()
            )
        } else {
            outcome.answer.clone()
        };
        let mut payload = EditPayload::new(diff_summary, outcome.files_changed.clone())
            .with_conversation_ref(conversation_ref);
        for (name, summary) in &outcome.tool_calls {
            payload = payload.with_tool_call(EditToolCall::new(name, summary));
        }
        let payload_value = payload
            .to_value()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;

        // §6.6 fork-on-divergence: a tip continues its branch; a non-tip auto-forks.
        let forked = !is_tip;
        let edit_branch_id = if forked {
            forked_branch_name(target_node_id)
        } else {
            branch_id.clone()
        };

        // Create the Edit node owning the new snapshot, parented on the target.
        let version = semver::Version::parse(EDIT_TYPE_VERSION)
            .map_err(|e| DaemonError::Graph(format!("bad edit version: {e}")))?;
        let (edit_node, schema_version) = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .create_node(
                    EDIT_KIND,
                    Some(&version),
                    vec![target_node_id],
                    &edit_branch_id,
                    payload_value,
                    true,
                    Some(new_snapshot),
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            (env.id, env.payload_schema_version)
        };
        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id: edit_node,
            schema_version,
        })?;
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: target_node_id,
            to: edit_node,
            edge: EdgeType::ParentChild,
        })?;

        // Move the branch ref (continue a tip) or create it (auto-fork a non-tip),
        // and point HEAD at the new Edit node (the current working state).
        if forked {
            let forked_ref = edit_branch_id.clone();
            self.publish_event(|seq| OpLogEvent::BranchForked {
                seq,
                ref_name: forked_ref,
            })?;
        }
        self.promote_ref(&edit_branch_id, edit_node)?;
        self.set_head_ref(edit_node)?;

        // Auto-run a change-scoped Sanity check against the Edit (DESIGN.md §8.2),
        // observing — a sanity failure never fails the edit.
        let sanity_config = serde_json::json!({ "forbid": ["FIXME", "XXX"] });
        let sanity =
            self.auto_run_sanity(edit_node, outcome.files_changed.clone(), sanity_config)?;
        let sanity_label = sanity.map(|run| run.envelope.outcome.label().to_string());

        Ok(self.record_mutation(serde_json::json!({
            "editNodeId": edit_node.to_string(),
            "branchId": edit_branch_id,
            "forked": forked,
            "model": outcome.model_label,
            "costMicroUsd": outcome.cost_micros,
            "sanity": sanity_label,
        })))
    }

    /// Build a [`WorktreeCowBackend`] over a second handle to the daemon's on-disk
    /// CAS, with its own asset cache, lease ledger, and worktree dir under the
    /// project's state root. The captured tree lands in the same store the graph
    /// reads from (content-addressing makes the two handles consistent).
    fn build_edit_backend(
        &self,
    ) -> Result<WorktreeCowBackend<LooseStore, LocalCasAssetStore>, DaemonError> {
        let state_root = self
            .cas_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cas_dir.clone());
        let objects = ObjectStore::new(
            LooseStore::open(&self.cas_dir).map_err(|e| DaemonError::Cas(e.to_string()))?,
        );
        let assets = LocalCasAssetStore::open(state_root.join("assets"))
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        let ledger = LeaseLedger::open(state_root.join("leases.json"))
            .map_err(|e| DaemonError::Io(format!("open lease ledger: {e}")))?;
        let worktree_dir = state_root.join("worktrees");
        std::fs::create_dir_all(&worktree_dir)
            .map_err(|e| DaemonError::Io(format!("create worktree dir {worktree_dir:?}: {e}")))?;
        Ok(WorktreeCowBackend::new(
            objects,
            assets,
            worktree_dir,
            ledger,
        ))
    }

    /// The bounded agent tool-loop over a provisioned CoW workspace. Calls
    /// [`run_turn`](spork_agent::run_turn) repeatedly, executing each tool call
    /// against `ws`, until the model stops emitting tool calls or a ceiling is hit.
    fn run_edit_loop(
        &self,
        backend: &dyn IsolationBackend,
        ws: &Workspace,
        agent_config: &crate::agent::AgentConfig,
        selector: &ModelSelector,
        privacy: spork_provider::PrivacyClass,
        prompt: &str,
    ) -> Result<EditLoopOutcome, DaemonError> {
        let transports = DaemonTransports(agent_config);
        let cancel = CancelToken::new();

        let mut turns = vec![
            CanonicalTurn::text(Role::System, TOOL_SYSTEM_PROMPT),
            CanonicalTurn::text(Role::User, prompt),
        ];
        let mut files: BTreeSet<String> = BTreeSet::new();
        let mut tool_calls: Vec<(String, String)> = Vec::new();
        let mut cost_micros: u64 = 0;
        let mut model_label = String::new();
        let mut answer = String::new();

        for iter in 0..MAX_ITERATIONS {
            let input = CanonicalTranscript::new(turns.clone());
            let result = match spork_agent::run_turn(
                &self.router,
                &transports,
                &self.accountant,
                selector,
                privacy,
                &input,
            ) {
                Ok(r) => r,
                // A first-turn failure (no transport / privacy refusal) is fatal;
                // a later failure stops the loop with the work captured so far.
                // The CLI-as-model misuse case fails loud with guidance (R1).
                Err(e) if iter == 0 => return Err(map_agent_error_for(e, agent_config)),
                Err(_) => break,
            };

            cost_micros = cost_micros.saturating_add(result.cost.micro_usd);
            model_label = format!("{}/{}", result.provider, result.model_key);

            // Fold the assistant turns into the running transcript and collect any
            // tool calls (id, name, input) to execute.
            let mut pending: Vec<(String, String, serde_json::Value)> = Vec::new();
            for turn in &result.output.turns {
                turns.push(turn.clone());
                for block in &turn.content {
                    match block {
                        ContentBlock::Text(text) => answer = text.clone(),
                        ContentBlock::ToolCall { id, name, input } => {
                            pending.push((id.clone(), name.clone(), input.clone()));
                        }
                        ContentBlock::ToolResult { .. } => {}
                    }
                }
            }

            if pending.is_empty() {
                break; // the model is done — the final text is the answer.
            }
            if cost_micros > MAX_COST_MICROS {
                break; // cost ceiling reached — stop before the next turn.
            }

            // Execute each tool call against the CoW workspace and append the
            // results as a single tool-role turn (the correlation ids round-trip).
            let mut result_blocks = Vec::with_capacity(pending.len());
            for (id, name, input) in pending {
                let exec = execute_tool(backend, ws, &name, &input, &cancel);
                if let Some(path) = exec.changed {
                    files.insert(path);
                }
                tool_calls.push((name.clone(), tool_summary(&name, &input)));
                result_blocks.push(ContentBlock::ToolResult {
                    tool_call_id: id,
                    content: exec.content,
                    is_error: exec.is_error,
                });
            }
            turns.push(CanonicalTurn {
                role: Role::Tool,
                content: result_blocks,
                tool_call_id: None,
                opaque: Vec::new(),
            });
        }

        Ok(EditLoopOutcome {
            transcript: CanonicalTranscript::new(turns),
            files_changed: files.into_iter().collect(),
            tool_calls,
            cost_micros,
            model_label,
            answer,
        })
    }
}

/// The result of executing one tool call against the workspace.
struct ToolExec {
    /// The tool-result content (carried back to the model).
    content: serde_json::Value,
    /// Whether the tool failed (the model can recover / retry).
    is_error: bool,
    /// The repo-relative path the tool wrote, if any (for the change scope).
    changed: Option<String>,
}

impl ToolExec {
    fn ok(content: serde_json::Value, changed: Option<String>) -> Self {
        ToolExec {
            content,
            is_error: false,
            changed,
        }
    }
    fn err(message: impl Into<String>) -> Self {
        ToolExec {
            content: serde_json::Value::String(message.into()),
            is_error: true,
            changed: None,
        }
    }
}

/// Execute one trusted tool call against the CoW workspace. All file ops are
/// confined to the workspace root (path escape is refused); `run_command` runs
/// through the isolation backend's `exec`.
fn execute_tool(
    backend: &dyn IsolationBackend,
    ws: &Workspace,
    name: &str,
    input: &serde_json::Value,
    cancel: &CancelToken,
) -> ToolExec {
    match name {
        "read_file" => {
            let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
                return ToolExec::err("read_file requires a string `path`");
            };
            match safe_join(&ws.root, rel) {
                Ok(path) => match std::fs::read_to_string(&path) {
                    Ok(text) => ToolExec::ok(serde_json::Value::String(text), None),
                    Err(e) => ToolExec::err(format!("read {rel}: {e}")),
                },
                Err(e) => ToolExec::err(e),
            }
        }
        "write_file" => {
            let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
                return ToolExec::err("write_file requires a string `path`");
            };
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            match safe_join(&ws.root, rel) {
                Ok(path) => {
                    if let Some(parent) = path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return ToolExec::err(format!("write {rel}: {e}"));
                        }
                    }
                    match std::fs::write(&path, content) {
                        Ok(()) => ToolExec::ok(
                            serde_json::json!({ "ok": true, "bytes": content.len() }),
                            Some(rel.to_string()),
                        ),
                        Err(e) => ToolExec::err(format!("write {rel}: {e}")),
                    }
                }
                Err(e) => ToolExec::err(e),
            }
        }
        "list_dir" => {
            let rel = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match safe_join(&ws.root, rel) {
                Ok(path) => match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        let names: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect();
                        ToolExec::ok(serde_json::json!(names), None)
                    }
                    Err(e) => ToolExec::err(format!("list {rel}: {e}")),
                },
                Err(e) => ToolExec::err(e),
            }
        }
        "run_command" => {
            let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
                return ToolExec::err("run_command requires a string `command`");
            };
            let args: Vec<String> = input
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let prepared = PreparedRun::new(command, args).with_cwd(ws.root.clone());
            match backend.exec(ws, prepared, cancel) {
                Ok(out) => ToolExec::ok(
                    serde_json::json!({
                        "exitCode": out.exit_code,
                        "stdout": String::from_utf8_lossy(&out.stdout),
                        "stderr": String::from_utf8_lossy(&out.stderr),
                    }),
                    None,
                ),
                Err(e) => ToolExec::err(format!("run_command {command}: {e}")),
            }
        }
        "apply_patch" => {
            let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
                return ToolExec::err("apply_patch requires a string `path`");
            };
            let Some(patch) = input.get("patch").and_then(|v| v.as_str()) else {
                return ToolExec::err("apply_patch requires a string `patch`");
            };
            match safe_join(&ws.root, rel) {
                Ok(path) => {
                    let original = std::fs::read_to_string(&path).unwrap_or_default();
                    match apply_unified_diff(&original, patch) {
                        Ok(updated) => match std::fs::write(&path, updated) {
                            Ok(()) => ToolExec::ok(
                                serde_json::json!({ "ok": true }),
                                Some(rel.to_string()),
                            ),
                            Err(e) => ToolExec::err(format!("apply_patch write {rel}: {e}")),
                        },
                        Err(e) => ToolExec::err(format!("apply_patch {rel}: {e}")),
                    }
                }
                Err(e) => ToolExec::err(e),
            }
        }
        other => ToolExec::err(format!("unknown tool {other:?}")),
    }
}

/// A short human summary of a tool call, for the Edit payload's `tool_calls`.
fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    match input.get("path").and_then(|v| v.as_str()) {
        Some(path) => format!("{name} {path}"),
        None => match input.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => format!("{name} {cmd}"),
            None => name.to_string(),
        },
    }
}

/// Join a repo-relative path onto the workspace root, refusing any escape
/// (absolute paths, `..`, or a root/prefix component) so a tool can never touch a
/// file outside the CoW copy (DESIGN.md §9.2 — the user's checkout is sacrosanct).
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(rel);
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err(format!("path escapes workspace: {rel:?}")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("absolute path not allowed: {rel:?}"))
            }
        }
    }
    Ok(root.join(candidate))
}

/// The auto-forked branch name for a non-tip edit: `agent/<id-suffix>`.
fn forked_branch_name(node_id: Ulid) -> String {
    let s = node_id.to_string();
    let suffix: String = s
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("agent/{suffix}")
}

/// Apply a unified-diff `patch` to `original`, returning the updated content.
///
/// A pragmatic, content-matching applier for agent-generated patches: it ignores
/// the (often-drifting) `@@` line numbers and instead locates each hunk's
/// context+removed block by content, replacing it with the context+added block.
/// A hunk that does not match is an error (the agent can fall back to
/// `write_file`). Good enough for the trusted MVP; not a full GNU-patch.
fn apply_unified_diff(original: &str, patch: &str) -> Result<String, String> {
    let hunks = parse_hunks(patch)?;
    if hunks.is_empty() {
        return Err("patch contained no hunks".to_string());
    }
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    for hunk in hunks {
        // `before` = context + removed lines (what must currently be present);
        // `after` = context + added lines (what replaces it).
        let before: Vec<&str> = hunk
            .iter()
            .filter(|(tag, _)| *tag != '+')
            .map(|(_, line)| line.as_str())
            .collect();
        let after: Vec<String> = hunk
            .iter()
            .filter(|(tag, _)| *tag != '-')
            .map(|(_, line)| line.clone())
            .collect();
        if before.is_empty() {
            return Err("patch hunk has no context to locate".to_string());
        }
        let pos = find_block(&lines, &before)
            .ok_or_else(|| "patch hunk did not match the file".to_string())?;
        lines.splice(pos..pos + before.len(), after);
    }
    let mut out = lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Parse a unified-diff body into hunks. Each hunk is a list of
/// `(tag, line)` where `tag` is `' '` (context), `'-'` (removed), or `'+'`
/// (added) and `line` is the content with the leading tag char stripped.
fn parse_hunks(patch: &str) -> Result<Vec<Vec<(char, String)>>, String> {
    let mut hunks: Vec<Vec<(char, String)>> = Vec::new();
    let mut current: Option<Vec<(char, String)>> = None;
    for raw in patch.lines() {
        if raw.starts_with("@@") {
            if let Some(h) = current.take() {
                hunks.push(h);
            }
            current = Some(Vec::new());
            continue;
        }
        // Ignore file headers (`---`/`+++`) and any preamble before the first @@.
        if raw.starts_with("---") || raw.starts_with("+++") {
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            continue;
        };
        let mut chars = raw.chars();
        match chars.next() {
            Some(' ') => hunk.push((' ', chars.as_str().to_string())),
            Some('-') => hunk.push(('-', chars.as_str().to_string())),
            Some('+') => hunk.push(('+', chars.as_str().to_string())),
            // A blank line inside a hunk is a context line of empty content.
            None => hunk.push((' ', String::new())),
            // Anything else (e.g. `\ No newline at end of file`) is ignored.
            Some(_) => {}
        }
    }
    if let Some(h) = current.take() {
        hunks.push(h);
    }
    Ok(hunks)
}

/// Find the first index where `block` appears as a contiguous run in `lines`.
fn find_block(lines: &[String], block: &[&str]) -> Option<usize> {
    if block.is_empty() || block.len() > lines.len() {
        return None;
    }
    (0..=lines.len() - block.len()).find(|&start| {
        lines[start..start + block.len()]
            .iter()
            .zip(block)
            .all(|(a, b)| a == b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_refuses_escape_and_absolute() {
        let root = Path::new("/ws");
        assert!(safe_join(root, "src/lib.rs").is_ok());
        assert!(safe_join(root, "./a/b.rs").is_ok());
        assert!(safe_join(root, "../etc/passwd").is_err());
        assert!(safe_join(root, "a/../../b").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
    }

    #[test]
    fn apply_unified_diff_replaces_a_matched_hunk() {
        let original = "line one\nline two\nline three\n";
        let patch = "@@ -1,3 +1,3 @@\n line one\n-line two\n+LINE TWO\n line three\n";
        let updated = apply_unified_diff(original, patch).unwrap();
        assert_eq!(updated, "line one\nLINE TWO\nline three\n");
    }

    #[test]
    fn apply_unified_diff_errors_on_a_non_matching_hunk() {
        let original = "alpha\nbeta\n";
        let patch = "@@ -1,1 +1,1 @@\n-gamma\n+delta\n";
        assert!(apply_unified_diff(original, patch).is_err());
    }

    #[test]
    fn forked_branch_name_is_agent_prefixed() {
        let id = Ulid::new();
        assert!(forked_branch_name(id).starts_with("agent/"));
    }
}
