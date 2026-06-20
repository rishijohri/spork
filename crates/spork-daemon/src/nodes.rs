//! P5 dispatch: observing checks (`node.runCheck` + the Edit auto-run hook) and
//! branch merge (`branch.merge`) over the P5 built-in node types.
//!
//! This is the additive P5 wiring behind the frozen F3 seams (CLAUDE.md C2/C3):
//! it adds new dispatch arms and an Edit auto-run hook *without* changing any
//! existing command, event, or contract. Every built-in runs through the *same*
//! public registry (F2) and the *same* Runner SPI (F4) a P8 plugin uses
//! (DESIGN.md §7.1, §8.1, §9).
//!
//! # `node.runCheck` (observing, never mutating)
//!
//! Running a check authorizes the required capability through the broker
//! (`snapshot.read`, plus `process.spawn` for a non-hermetic Validation/Stress
//! runner), materializes the target node's snapshot into a throwaway worktree,
//! runs the check through the F4 Runner SPI with the content-addressed result
//! cache in the loop, stores the append-only [`ResultEnvelope`](spork_runner::ResultEnvelope)
//! as a CAS blob, and creates an **observing** result node attached to the target
//! via a typed edge ([`Validates`](spork_graph::EdgeType::Validates) /
//! [`Checks`](spork_graph::EdgeType::Checks) /
//! [`Stresses`](spork_graph::EdgeType::Stresses)). **The target's snapshot is
//! never mutated** — the result is an observing child (DESIGN.md §6.2, §8.1).
//! Emits [`OpLogEvent::ResultRecorded`](spork_ipc::OpLogEvent::ResultRecorded).
//!
//! # The Edit auto-run hook
//!
//! When an Edit node is created, [`Daemon::auto_run_sanity`] schedules a Sanity
//! check against it, **change-scoped** to the Edit's `files_changed` and
//! cache-hitting on an unchanged subtree via the F4 `input_digest` (DESIGN.md
//! §8.2). It emits [`OpLogEvent::CheckScheduled`](spork_ipc::OpLogEvent::CheckScheduled)
//! then [`OpLogEvent::ResultRecorded`](spork_ipc::OpLogEvent::ResultRecorded).
//!
//! # `branch.merge` (3-way reconciliation)
//!
//! Computing the nearest common ancestor and reconciling the two sides via the P5
//! [`three_way_merge`](spork_nodes::three_way_merge) yields either a clean,
//! materializable Merge node (owning a merged snapshot) — emitting
//! [`OpLogEvent::MergePerformed`](spork_ipc::OpLogEvent::MergePerformed) and
//! re-running the merge-parents' observing checks against the merged node so the
//! merge is certified by post-merge results (DESIGN.md A.4) — or a conflict set
//! returned in the reply with **no half-node built** (DESIGN.md §6.5, A.4).
//!
//! Design references: DESIGN.md §6.2, §6.3, §6.5, §7.1, §8.1, §8.2, A.4.

use std::collections::BTreeMap;

use spork_broker::{Capability, RequestedScope};
use spork_exec::{CancelToken, Lease, Workspace};
use spork_graph::EdgeType;
use spork_hash::Hash;
use spork_ipc::{CommandResult, OpLogEvent};
use spork_nodes::{three_way_merge, three_way_merge_with_resolution, MergeOutcome, MergePayload};
use spork_runner::{
    input_digest, ChangeScope, CheckSpec, DerivationKey, ResultCache, ResultEnvelope, Runner,
    SandboxContext,
};
use ulid::Ulid;

use crate::core::{Daemon, WORKTREE_GLOB};
use crate::error::DaemonError;

/// The semver string the daemon stamps on the P5 built-in nodes it creates
/// (matches [`spork_nodes::type_version`]).
const BUILTIN_TYPE_VERSION: &str = "1.0.0";

/// The outcome of running one check: the normalized envelope plus whether it was
/// served from cache (so a caller/test can prove the cache-hit DoD).
pub(crate) struct CheckRunResult {
    /// The normalized result envelope.
    pub(crate) envelope: ResultEnvelope,
    /// Whether the runner actually executed (`true`) or the cache served it
    /// (`false`).
    pub(crate) executed: bool,
    /// The observing result node attached to the target.
    pub(crate) result_node: Ulid,
}

impl Daemon {
    /// `node.runCheck`: run an observing check against a node and attach an
    /// append-only result without mutating the node (DESIGN.md §8.1).
    ///
    /// The `spec` is `{ "check": "<kind>", "config": { ... },
    /// "changed_paths": ["..."]? }` — `check` selects the built-in runner, `config`
    /// is the runner's opaque config, and the optional `changed_paths` scopes the
    /// run (DESIGN.md §8.2). Returns a mutation reply whose `ids` carry the new
    /// result node id, the run id, and whether the result was a cache hit.
    pub(crate) fn cmd_node_run_check(
        &self,
        target_node_id: Ulid,
        spec: serde_json::Value,
    ) -> Result<CommandResult, DaemonError> {
        let parsed = ParsedCheckSpec::parse(&spec)?;
        let run = self.run_check_internal(target_node_id, &parsed)?;
        Ok(self.record_mutation(serde_json::json!({
            "resultNodeId": run.result_node.to_string(),
            "runId": run.envelope.run_id.to_string(),
            "cacheHit": !run.executed,
            "outcome": run.envelope.outcome.label(),
        })))
    }

    /// If `kind` is the Edit built-in, auto-run a change-scoped Sanity check
    /// against the just-created node (the `onEditNodeCommitted` hook, DESIGN.md
    /// §8.2); otherwise a no-op.
    ///
    /// The change scope is the Edit payload's `files_changed`; the sanity rule set
    /// is the payload's optional `sanity_config` (a default forbid-pattern check
    /// when absent, so the auto-run does something useful out of the box). This is
    /// best-effort: a check failure never fails the Edit creation that triggered
    /// it (the result is observing — DESIGN §8.1).
    pub(crate) fn maybe_auto_run_sanity(
        &self,
        node_id: Ulid,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<(), DaemonError> {
        if kind != spork_nodes::EDIT_KIND {
            return Ok(());
        }
        let files_changed: Vec<String> = payload
            .get("files_changed")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        // The Edit may carry an explicit sanity rule set; otherwise use a
        // conservative default that flags leftover stub markers.
        let sanity_config = payload
            .get("sanity_config")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "forbid": ["TODO", "FIXME", "XXX"] }));
        let _ = self.auto_run_sanity(node_id, files_changed, sanity_config)?;
        Ok(())
    }

    /// The Edit auto-run hook: schedule a change-scoped Sanity check against a
    /// freshly created Edit node (DESIGN.md §8.2, `onEditNodeCommitted`).
    ///
    /// `files_changed` is the Edit's change scope; the Sanity check is scoped to
    /// it so an unchanged subtree cache-hits via the F4 `input_digest`. Emits
    /// `CHECK_SCHEDULED` (before running) then `RESULT_RECORDED` (after). This is
    /// best-effort and observing: a failure to auto-run never fails the Edit
    /// creation that triggered it, so the hook returns `Ok(None)` on a
    /// non-fatal skip (e.g. the Edit owns no snapshot to scan).
    pub(crate) fn auto_run_sanity(
        &self,
        edit_node_id: Ulid,
        files_changed: Vec<String>,
        sanity_config: serde_json::Value,
    ) -> Result<Option<CheckRunResult>, DaemonError> {
        let parsed = ParsedCheckSpec {
            kind: spork_nodes::SANITY_NODE_KIND.to_string(),
            config: sanity_config,
            changed_paths: files_changed,
        };
        // Announce the scheduled check first so a renderer can show it pending,
        // then run it and announce the result (DESIGN §8.2).
        let run_id = Ulid::new();
        self.publish_event(|seq| OpLogEvent::CheckScheduled { seq, run_id })?;
        let run = self.run_check_internal(edit_node_id, &parsed)?;
        Ok(Some(run))
    }

    /// The shared internals of running a check: authorize, materialize, run
    /// through the cache, store the envelope, create the observing node, emit
    /// `RESULT_RECORDED`.
    fn run_check_internal(
        &self,
        target_node_id: Ulid,
        parsed: &ParsedCheckSpec,
    ) -> Result<CheckRunResult, DaemonError> {
        let runner = spork_nodes::runner_for(&parsed.kind).ok_or_else(|| {
            DaemonError::Graph(format!(
                "no built-in runner for check kind {:?}",
                parsed.kind
            ))
        })?;
        let caps = runner.describe();

        // Authorize: a check always reads the snapshot; a non-hermetic runner
        // (Validation/Stress) additionally spawns a process (DESIGN §8.2, §15.2).
        self.authorize(
            Capability::SnapshotRead,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        if !caps.hermetic {
            self.authorize(
                Capability::ProcessSpawn,
                &RequestedScope::path(WORKTREE_GLOB),
            )?;
        }

        // Resolve the target's snapshot root tree (it must own a snapshot to be
        // observed) and materialize it into a throwaway worktree.
        let (root_tree, parent_snapshot_hash) = self.node_root_tree_and_snapshot(target_node_id)?;
        let scratch = tempfile::tempdir()
            .map_err(|e| DaemonError::Io(format!("create check worktree: {e}")))?;
        {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            core.store
                .materialize_tree(&root_tree, scratch.path())
                .map_err(|e| DaemonError::Cas(e.to_string()))?;
        }

        let spec = CheckSpec::new(parsed.kind.clone(), parsed.config.clone());
        let ctx = SandboxContext {
            workspace: scratch_workspace(scratch.path(), parent_snapshot_hash),
            input_tree: root_tree,
            changed_paths: parsed.changed_paths.clone(),
        };

        // Run through the content-addressed cache so an identical re-run (same
        // spec, tree, runner, scope) cache-hits without re-executing (DESIGN §8.1,
        // §8.2, §9.2).
        let (envelope, executed) = self.run_with_cache(runner.as_ref(), &ctx, &spec)?;

        // Store the append-only ResultEnvelope as a content-addressed CAS blob
        // (the small queryable result; bulky artifacts are referenced by hash).
        let _envelope_ref = self.store_envelope(&envelope)?;

        // Create the OBSERVING result node attached to the target — never mutating
        // the parent (owns_snapshot = false, no snapshot_hash). The edge type is
        // the observing relation for this check kind.
        let result_node = self.create_observing_node(target_node_id, parsed, &envelope)?;

        self.publish_event(|seq| OpLogEvent::ResultRecorded {
            seq,
            run_id: envelope.run_id,
            node_id: result_node,
        })?;

        Ok(CheckRunResult {
            envelope,
            executed,
            result_node,
        })
    }

    /// Run a boxed runner through the daemon's result cache, returning the
    /// envelope and whether the runner actually executed (a miss) or the cache
    /// served it (a hit).
    ///
    /// Mirrors the F4 [`CachingRunner`](spork_runner::CachingRunner) policy for a
    /// `dyn Runner`: build the [`DerivationKey`] from the input tree, canonical
    /// config, runner version, and the runner's formula generation; on a hit
    /// return the stored run unchanged (append-only history preserved); on a miss
    /// run prepare→run→normalize, stamp the real `input_digest`, store it, and
    /// return it. An impure spec bypasses the cache entirely.
    fn run_with_cache(
        &self,
        runner: &dyn Runner,
        ctx: &SandboxContext,
        spec: &CheckSpec,
    ) -> Result<(ResultEnvelope, bool), DaemonError> {
        let caps = runner.describe();
        let scope = ChangeScope::from_paths(&ctx.changed_paths);
        let digest = input_digest(&ctx.input_tree, spec, &caps.version, &scope)
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        let key = DerivationKey::new(digest, caps.generation);

        if !spec.impure {
            if let Some(envelope) = self
                .result_cache
                .get(&key)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
            {
                // Cache hit: return the stored run, no re-execution.
                return Ok((envelope, false));
            }
        }

        // Cache miss (or impure): run the SPI and stamp the real input digest.
        let signal = CancelToken::new();
        let prepared = runner
            .prepare(ctx, spec)
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        let raw = runner
            .run(prepared, &signal)
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        let mut envelope = runner
            .normalize(raw, spec)
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        envelope.input_digest = digest;

        if !spec.impure {
            self.result_cache
                .put(&key, &envelope)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
        }
        Ok((envelope, true))
    }

    /// Store a [`ResultEnvelope`] as a content-addressed CAS blob, returning its
    /// content hash (the append-only result store, DESIGN.md §8.2).
    fn store_envelope(&self, envelope: &ResultEnvelope) -> Result<Hash, DaemonError> {
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| DaemonError::Cas(format!("encode result envelope: {e}")))?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let (hash, _stats) = core
            .store
            .put_blob_bytes(&bytes)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok(hash)
    }

    /// Create the observing result node attached to `target` with the right edge,
    /// folding the latest outcome into the node's lifecycle status. Never sets a
    /// snapshot — an observing node owns none (DESIGN.md §6.2, §8.1).
    fn create_observing_node(
        &self,
        target: Ulid,
        parsed: &ParsedCheckSpec,
        envelope: &ResultEnvelope,
    ) -> Result<Ulid, DaemonError> {
        let version = semver::Version::parse(BUILTIN_TYPE_VERSION)
            .map_err(|e| DaemonError::Graph(format!("bad builtin version: {e}")))?;
        let payload = serde_json::json!({
            "schema_version": 1,
            "target_node_id": target.to_string(),
            "check": parsed.kind,
            "outcome": envelope.outcome.label(),
            "run_id": envelope.run_id.to_string(),
        });
        let edge = observing_edge_for(&parsed.kind);

        let node_id = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            // The observing node is created on the same branch as its target.
            let branch_id = core
                .graph
                .get_node(target)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .map(|env| env.branch_id)
                .unwrap_or_else(|| "main".to_string());
            let env = core
                .graph
                .create_node(
                    &parsed.kind,
                    Some(&version),
                    vec![target],
                    &branch_id,
                    payload,
                    false, // observing: owns NO snapshot, never mutates the parent
                    None,
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            let node_id = env.id;
            // The PARENT_CHILD relation (result -> target) is materialized by
            // `create_node` from `parent_ids`. Add the typed observing edge, which
            // a test-class observing node *originates* toward the Edit it observes
            // (DESIGN §6.3) — so it goes from the result node to the target. The
            // observing node's descriptor declares this edge in `allowed_edges`.
            core.graph
                .add_edge(node_id, target, edge)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            node_id
        };

        // Publish the node + its edges onto the ordered rail (the result state
        // arrives via events, never the return value). The PARENT_CHILD edge runs
        // result -> target (child -> parent), matching the projection's relation.
        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version: 1,
        })?;
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: node_id,
            to: target,
            edge: EdgeType::ParentChild,
        })?;
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: node_id,
            to: target,
            edge,
        })?;
        Ok(node_id)
    }

    /// `branch.merge`: 3-way reconciliation of `from_node_id` into the head of
    /// `into_ref`, producing a clean Merge node or a conflict set (DESIGN.md
    /// §6.5, A.4).
    pub(crate) fn cmd_branch_merge(
        &self,
        into_ref: &str,
        from_node_id: Ulid,
        resolution: Option<serde_json::Value>,
    ) -> Result<CommandResult, DaemonError> {
        // A merge writes a new snapshot-owning node — a snapshot.write op.
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        // Resolve "ours" (the head of into_ref) and "theirs" (from_node_id), plus
        // their nearest common ancestor.
        let ours = self.resolve_ref_head(into_ref)?;
        let theirs = from_node_id;
        let base = self.nearest_common_ancestor(ours, theirs)?;

        let ours_tree = self.node_root_tree_only(ours)?;
        let theirs_tree = self.node_root_tree_only(theirs)?;
        let base_tree = match base {
            Some(b) => self.node_root_tree_only(b)?,
            // No common ancestor: reconcile against the empty tree (everything is
            // an add). Materialize an empty tree so the merge has a base.
            None => self.empty_tree()?,
        };

        // Reconcile, applying a pre-supplied resolution if one was given.
        let outcome = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            match &resolution {
                Some(r) => {
                    let resolution = parse_resolution(r)?;
                    three_way_merge_with_resolution(
                        &core.store,
                        &base_tree,
                        &ours_tree,
                        &theirs_tree,
                        &resolution,
                    )
                    .map_err(|e| DaemonError::Graph(e.to_string()))?
                }
                None => three_way_merge(&core.store, &base_tree, &ours_tree, &theirs_tree)
                    .map_err(|e| DaemonError::Graph(e.to_string()))?,
            }
        };

        match outcome {
            MergeOutcome::Conflicts(conflicts) => {
                // No half-node: return the conflict set as data (DESIGN A.4).
                let conflict_json: Vec<serde_json::Value> = conflicts
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "path": c.path,
                            "base": c.base.map(|h| h.to_hex()),
                            "ours": c.ours.map(|h| h.to_hex()),
                            "theirs": c.theirs.map(|h| h.to_hex()),
                        })
                    })
                    .collect();
                Ok(self.record_mutation(serde_json::json!({
                    "merged": false,
                    "conflicts": conflict_json,
                })))
            }
            MergeOutcome::Clean {
                merged_tree,
                resolution: res,
            } => {
                let merge_node =
                    self.create_merge_node(into_ref, ours, theirs, base_tree, merged_tree, &res)?;
                self.publish_event(|seq| OpLogEvent::MergePerformed {
                    seq,
                    node_id: merge_node,
                })?;
                // Re-run the merge-parents' observing checks against the merged
                // snapshot so the merge is certified by post-merge results, never
                // stale pre-merge ones (DESIGN A.4 observing carry-over).
                self.rerun_observers_against_merge(&[ours, theirs], merge_node)?;
                Ok(self.record_mutation(serde_json::json!({
                    "merged": true,
                    "mergeNodeId": merge_node.to_string(),
                })))
            }
        }
    }

    /// Create the materializable Merge node: wrap the merged tree in a Snapshot
    /// object, create a `merge` node owning it with both parents, attach the
    /// merge edges, move `into_ref`, and publish the node + edges.
    #[allow(clippy::too_many_arguments)]
    fn create_merge_node(
        &self,
        into_ref: &str,
        ours: Ulid,
        theirs: Ulid,
        base_tree: Hash,
        merged_tree: Hash,
        resolution: &spork_merge::ConflictResolution,
    ) -> Result<Ulid, DaemonError> {
        let version = semver::Version::parse(BUILTIN_TYPE_VERSION)
            .map_err(|e| DaemonError::Graph(format!("bad builtin version: {e}")))?;
        let payload = MergePayload::new(base_tree, resolution.clone());
        let payload_value = payload
            .to_value()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;

        // Wrap the merged tree in a Snapshot object so the node binds a snapshot
        // hash (not a bare tree), matching every other mutating node.
        let snapshot_hash = self.put_snapshot_over(merged_tree)?;

        let node_id = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            let branch_id = core
                .graph
                .get_node(ours)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .map(|env| env.branch_id)
                .unwrap_or_else(|| into_ref.to_string());
            // The first parent is recorded via `create_node` (which materializes a
            // PARENT_CHILD edge merge -> ours). The second-and-later parents are
            // MERGE_PARENT, added explicitly (DESIGN §6.3) — `create_node` cannot
            // distinguish them, so passing only `ours` keeps the first edge
            // PARENT_CHILD and the merge edge MERGE_PARENT. The projection folds
            // both into the merge node's `parent_ids`.
            let env = core
                .graph
                .create_node(
                    spork_nodes::MERGE_KIND,
                    Some(&version),
                    vec![ours],
                    &branch_id,
                    payload_value,
                    true, // a Merge node owns its merged snapshot
                    Some(snapshot_hash),
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            let node_id = env.id;
            // The additional merge parent: merge -> theirs (MERGE_PARENT). The
            // merge descriptor declares MERGE_PARENT in `allowed_edges`.
            core.graph
                .add_edge(node_id, theirs, EdgeType::MergeParent)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            // Move the target ref to the merge node.
            core.graph
                .move_ref(into_ref, node_id)
                .map_err(|e| {
                    // The ref may not exist yet (a merge onto a fresh ref); create
                    // it instead so the merge always lands somewhere.
                    DaemonError::Graph(e.to_string())
                })
                .or_else(|_| {
                    core.graph
                        .create_ref(into_ref, spork_graph::RefKind::Branch, node_id)
                        .map_err(|e| DaemonError::Graph(e.to_string()))
                })?;
            node_id
        };

        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version: 1,
        })?;
        // Edges run child -> parent (the projection's relation): merge ->
        // ours (PARENT_CHILD) and merge -> theirs (MERGE_PARENT).
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: node_id,
            to: ours,
            edge: EdgeType::ParentChild,
        })?;
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: node_id,
            to: theirs,
            edge: EdgeType::MergeParent,
        })?;
        let moved = into_ref.to_string();
        self.publish_event(|seq| OpLogEvent::RefMoved {
            seq,
            ref_name: moved,
            to: node_id,
        })?;
        Ok(node_id)
    }

    /// Re-run every observing check kind attached to `parents` against the merged
    /// snapshot (DESIGN.md A.4: observing results do not merge; they re-run against
    /// the merged tree).
    ///
    /// Re-running attaches a fresh observing result to the *merge node* (the new
    /// state being certified), so the merge's gate evaluates post-merge results,
    /// never the pre-merge ones (DESIGN.md A.4 gate interaction). The pre-merge
    /// results stay attached to their original targets, untouched (append-only
    /// history). NOTE: this does not flip the pre-merge observers' `is_stale` flag
    /// — the F2 `GraphService` exposes no staleness-write seam in this phase — but
    /// the post-merge re-run is what actually prevents a stale green, which is the
    /// substantive A.4 guarantee. This is best-effort: a re-run failure does not
    /// undo the completed merge.
    fn rerun_observers_against_merge(
        &self,
        parents: &[Ulid],
        merge_node: Ulid,
    ) -> Result<(), DaemonError> {
        // Collect the distinct observing-check kinds attached to either parent.
        let mut kinds: BTreeMap<String, ()> = BTreeMap::new();
        {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            let state = core
                .graph
                .projection()
                .state()
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            for parent in parents {
                for env in state.nodes.values() {
                    if env.parent_ids.contains(parent)
                        && env.family == spork_graph::Family::Observing
                    {
                        kinds.insert(env.kind.clone(), ());
                    }
                }
            }
        }

        // Re-run each observing kind against the merged node so the merge is
        // certified by post-merge results (DESIGN A.4).
        for kind in kinds.into_keys() {
            let parsed = ParsedCheckSpec {
                kind,
                config: serde_json::json!({}),
                changed_paths: Vec::new(),
            };
            // Best-effort: a re-run error does not roll back the merge.
            let _ = self.run_check_internal(merge_node, &parsed);
        }
        Ok(())
    }

    // ---- helpers -----------------------------------------------------------

    /// Resolve a node's snapshot root tree hash and the snapshot-object hash.
    fn node_root_tree_and_snapshot(&self, node_id: Ulid) -> Result<(Hash, Hash), DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let env = core
            .graph
            .get_node(node_id)
            .map_err(|e| DaemonError::Graph(e.to_string()))?
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
        let snapshot_hash = env
            .snapshot_hash
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id} owns no snapshot")))?;
        let snapshot = core
            .store
            .read_snapshot(&snapshot_hash)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok((snapshot.root_tree, snapshot_hash))
    }

    /// Resolve just a node's snapshot root tree hash.
    fn node_root_tree_only(&self, node_id: Ulid) -> Result<Hash, DaemonError> {
        Ok(self.node_root_tree_and_snapshot(node_id)?.0)
    }

    /// Wrap a root tree in a Snapshot object (under the daemon's ignore profile
    /// hash) and return the snapshot-object hash.
    fn put_snapshot_over(&self, root_tree: Hash) -> Result<Hash, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let ignore_hash = core.ignore_profile.hash();
        let (hash, _stats) = core
            .store
            .put_snapshot(root_tree, ignore_hash, None)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok(hash)
    }

    /// Materialize an empty root tree into the store (the merge base when no
    /// common ancestor exists).
    fn empty_tree(&self) -> Result<Hash, DaemonError> {
        let scratch = tempfile::tempdir()
            .map_err(|e| DaemonError::Io(format!("create empty-tree scratch: {e}")))?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let (hash, _stats) = core
            .store
            .put_tree(scratch.path(), &core.matcher)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        Ok(hash)
    }

    /// Resolve the node a ref points at, or the node directly if `into_ref` is a
    /// ULID string (a merge into a node rather than a named ref).
    fn resolve_ref_head(&self, into_ref: &str) -> Result<Ulid, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        if let Some(target) = core
            .graph
            .projection()
            .ref_target(into_ref)
            .map_err(|e| DaemonError::Graph(e.to_string()))?
        {
            return Ok(target);
        }
        // Fall back to interpreting into_ref as a node id.
        Ulid::from_string(into_ref)
            .map_err(|_| DaemonError::NotFound(format!("ref or node {into_ref:?}")))
            .and_then(|id| {
                if core
                    .graph
                    .get_node(id)
                    .map_err(|e| DaemonError::Graph(e.to_string()))?
                    .is_some()
                {
                    Ok(id)
                } else {
                    Err(DaemonError::NotFound(format!("ref or node {into_ref:?}")))
                }
            })
    }

    /// The nearest common ancestor of two nodes, walking `parent_ids` over the
    /// projection (DESIGN.md §6.5: three-way against the nearest common
    /// ancestor). Returns `None` if the two share no ancestor.
    fn nearest_common_ancestor(&self, a: Ulid, b: Ulid) -> Result<Option<Ulid>, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let state = core
            .graph
            .projection()
            .state()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
        let parents_of = |id: Ulid| -> Vec<Ulid> {
            state
                .nodes
                .get(&id.to_string())
                .map(|env| env.parent_ids.clone())
                .unwrap_or_default()
        };

        // Ancestors of `a` (inclusive), as a set with BFS-depth order.
        let a_ancestors = ancestors_inclusive(a, &parents_of);
        // BFS from `b`; the first node also in `a`'s ancestor set is a nearest
        // common ancestor (closest to `b`, which is sufficient for a clean base).
        let mut frontier = vec![b];
        let mut seen: BTreeMap<Ulid, ()> = BTreeMap::new();
        while let Some(node) = frontier.pop() {
            if seen.insert(node, ()).is_some() {
                continue;
            }
            if a_ancestors.contains_key(&node) {
                return Ok(Some(node));
            }
            for p in parents_of(node) {
                frontier.push(p);
            }
        }
        Ok(None)
    }
}

/// Build a [`Workspace`] over a materialized scratch directory.
///
/// The merge/check worktree is a throwaway CoW-equivalent copy; the lease is a
/// short-lived, daemon-owned, non-durable lease (the worktree is dropped with the
/// temp dir at the end of the dispatch, so no reaper bookkeeping is needed).
fn scratch_workspace(root: &std::path::Path, snapshot: Hash) -> Workspace {
    Workspace {
        root: root.to_path_buf(),
        snapshot,
        env_manifest_hash: Hash::from_bytes([0; 32]),
        lease: Lease {
            schema_version: 1,
            id: Ulid::new(),
            owner_pid: std::process::id(),
            ttl_ms: 60_000,
            heartbeat_ms: 10_000,
            durable: false,
            last_heartbeat_ms: 0,
        },
    }
}

/// The observing edge type a check kind originates to its target (DESIGN §6.3).
fn observing_edge_for(kind: &str) -> EdgeType {
    match kind {
        spork_nodes::VALIDATION_KIND => EdgeType::Validates,
        spork_nodes::STRESS_KIND => EdgeType::Stresses,
        // Sanity (and any other observing check) targets via CHECKS.
        _ => EdgeType::Checks,
    }
}

/// The set of a node's ancestors (inclusive of itself).
fn ancestors_inclusive(start: Ulid, parents_of: &impl Fn(Ulid) -> Vec<Ulid>) -> BTreeMap<Ulid, ()> {
    let mut out: BTreeMap<Ulid, ()> = BTreeMap::new();
    let mut frontier = vec![start];
    while let Some(node) = frontier.pop() {
        if out.insert(node, ()).is_some() {
            continue;
        }
        for p in parents_of(node) {
            frontier.push(p);
        }
    }
    out
}

/// Parse a [`ConflictResolution`](spork_merge::ConflictResolution) from a JSON
/// value (the optional `resolution` of `branch.merge`).
fn parse_resolution(
    value: &serde_json::Value,
) -> Result<spork_merge::ConflictResolution, DaemonError> {
    serde_json::from_value(value.clone())
        .map_err(|e| DaemonError::Graph(format!("invalid merge resolution: {e}")))
}

/// A parsed `node.runCheck` spec.
struct ParsedCheckSpec {
    /// The check kind (selects the built-in runner).
    kind: String,
    /// The runner's opaque config.
    config: serde_json::Value,
    /// The change scope (empty = whole tree).
    changed_paths: Vec<String>,
}

impl ParsedCheckSpec {
    /// Parse the `{ check, config, changed_paths? }` spec value.
    fn parse(spec: &serde_json::Value) -> Result<Self, DaemonError> {
        let kind = spec
            .get("check")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                DaemonError::Graph("runCheck spec requires a string `check` kind".to_string())
            })?
            .to_string();
        let config = spec.get("config").cloned().unwrap_or(serde_json::json!({}));
        let changed_paths = spec
            .get("changed_paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ParsedCheckSpec {
            kind,
            config,
            changed_paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_check_spec_reads_kind_config_and_scope() {
        let spec = serde_json::json!({
            "check": "sanity",
            "config": { "forbid": ["FIXME"] },
            "changed_paths": ["src/a.rs", "src/b.rs"]
        });
        let parsed = ParsedCheckSpec::parse(&spec).unwrap();
        assert_eq!(parsed.kind, "sanity");
        assert_eq!(parsed.config["forbid"][0], "FIXME");
        assert_eq!(parsed.changed_paths, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn parse_check_spec_defaults_config_and_scope() {
        let spec = serde_json::json!({ "check": "sanity" });
        let parsed = ParsedCheckSpec::parse(&spec).unwrap();
        assert_eq!(parsed.config, serde_json::json!({}));
        assert!(parsed.changed_paths.is_empty());
    }

    #[test]
    fn parse_check_spec_requires_a_kind() {
        let spec = serde_json::json!({ "config": {} });
        assert!(ParsedCheckSpec::parse(&spec).is_err());
    }

    #[test]
    fn observing_edge_for_each_check_kind() {
        assert_eq!(observing_edge_for("validation"), EdgeType::Validates);
        assert_eq!(observing_edge_for("stress"), EdgeType::Stresses);
        assert_eq!(observing_edge_for("sanity"), EdgeType::Checks);
        assert_eq!(observing_edge_for("custom"), EdgeType::Checks);
    }
}
