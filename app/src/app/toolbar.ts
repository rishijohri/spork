// Top-bar action toolbar (DESIGN.md §14.2, §14.5).
//
// Spork is a general codebase-editing IDE over a content-addressed work-DAG, so
// the toolbar's actions are the operations you perform on a selected node of that
// DAG: View, Analyze, Restore, Validate, New Branch, Commit to GitHub, Push to
// GitHub, Metadata. Each action declares `enabledWhen(node)` so a node exposes
// only the actions valid for its kind/family — exactly the schema-driven gating
// §14.5 requires. With no selection, only the selection-independent actions are
// enabled.
//
// Each MUTATING / action command also declares `buildCommand(node, ctx)`: the
// frozen `spork_ipc::Command` it dispatches (A.1, src/ipc/types.ts).
// `runToolbarAction` is the single wired path the panels call — it dispatches the
// command, and for a graph MUTATION registers the returned `opId` as an
// optimistic op so the op-log stream reconciles it (the store's `ingestEvent` +
// `resolveOptimistic`, DESIGN.md §14.4). The git actions (Commit/Push to GitHub)
// are action-shaped: they reply inline with a `GIT` result and emit no op-log
// event, so `runToolbarAction` returns `{opId:null}` for them. Read-only actions
// (View / Analyze / Metadata) carry no command and are local-only UI selections,
// so they never touch the daemon.

import type { Command, NodeView, Ulid } from "../ipc/types";
import { dispatch } from "../ipc/client";

/** The context a command builder needs beyond the selected node. */
export interface ToolbarContext {
  /** The default model the top-bar selector applies to new nodes. */
  defaultModel: string;
}

/** A single toolbar action with its enablement + (optional) command builder. */
export interface ToolbarAction {
  id: string;
  label: string;
  /** Whether the action is enabled for the given selection (null = none). */
  enabledWhen: (node: NodeView | null) => boolean;
  /**
   * The frozen `Command` this action dispatches, or `null` for a read-only /
   * local-only action (View, Analyze, Metadata) that has no daemon mutation.
   */
  buildCommand?: (node: NodeView | null, ctx: ToolbarContext) => Command | null;
}

/** True iff a node exists and owns a restorable snapshot. */
function isMaterializable(node: NodeView | null): node is NodeView {
  return node !== null && node.ownsSnapshot;
}

/** True iff a node exists at all (any selection). */
function hasSelection(node: NodeView | null): node is NodeView {
  return node !== null;
}

/** The frozen toolbar action set, in display order. */
export const TOOLBAR_ACTIONS: readonly ToolbarAction[] = [
  // View the selected node's state — any selected node. Read-only: opening the
  // details/diff surface is local UI, no daemon mutation.
  { id: "view", label: "View", enabledWhen: hasSelection, buildCommand: () => null },
  // Analyze — observing-style read over any selected node. Local-only here; a
  // richer slice keys an `Analyze` runner off the node payload.
  {
    id: "analyze",
    label: "Analyze",
    enabledWhen: hasSelection,
    buildCommand: () => null,
  },
  // Restore — only meaningful on a mutating, snapshot-owning node. Restores the
  // node's snapshot+conversation (A.1 `node.restore`), bringing the working tree
  // back to that node's state (DESIGN.md §6.4 — restore is an event, never an
  // overwrite; forward history survives as a branch).
  {
    id: "restore",
    label: "Restore",
    enabledWhen: (n) => isMaterializable(n) && n.family === "mutating",
    buildCommand: (n) =>
      n ? { command: "NODE_RESTORE", nodeId: n.id } : null,
  },
  // Validate — run a validation check against a snapshot-owning node
  // (A.1 `node.runCheck`, DESIGN.md §6.5 / P5 observing types).
  {
    id: "validate",
    label: "Validate",
    enabledWhen: isMaterializable,
    buildCommand: (n) =>
      n
        ? {
            command: "NODE_RUN_CHECK",
            targetNodeId: n.id,
            spec: { kind: "validation" },
          }
        : null,
  },
  // New Branch — fork a new branch off the selected node so you can explore an
  // alternative line of work from that point (A.1 `branch.fork`; metadata-only,
  // zero bytes copied, DESIGN.md §6.3). Available on any selection.
  {
    id: "newBranch",
    label: "New Branch",
    enabledWhen: hasSelection,
    buildCommand: (n) =>
      n
        ? { command: "BRANCH_FORK", fromNodeId: n.id, name: `branch/${n.id}` }
        : null,
  },
  // Commit to GitHub — project the selected node's snapshot into a real Git
  // commit on a new branch (A.1 `git.export`, DESIGN.md §10.4). Needs a snapshot
  // to commit, so it gates on `isMaterializable`. Action-shaped: replies inline
  // with a `GIT` result and emits no op-log event.
  {
    id: "gitExport",
    label: "Commit to GitHub",
    enabledWhen: isMaterializable,
    buildCommand: (n) =>
      n ? { command: "GIT_EXPORT", nodeId: n.id, branch: null } : null,
  },
  // Push to GitHub — export (if needed) then push the node's branch to a remote
  // using the user's existing git credentials (A.1 `git.push`, DESIGN.md §10.4).
  // Needs a snapshot to push, so it gates on `isMaterializable`. Action-shaped.
  {
    id: "gitPush",
    label: "Push to GitHub",
    enabledWhen: isMaterializable,
    buildCommand: (n) =>
      n ? { command: "GIT_PUSH", nodeId: n.id, remote: null } : null,
  },
  // Metadata — inspect/edit metadata of any selected node. Local-only (the
  // details panel surfaces it); no daemon mutation.
  {
    id: "metadata",
    label: "Metadata",
    enabledWhen: hasSelection,
    buildCommand: () => null,
  },
];

/** Look up an action by id (panels share the one frozen set). */
export function toolbarActionById(id: string): ToolbarAction | undefined {
  return TOOLBAR_ACTIONS.find((a) => a.id === id);
}

/** The result of running a toolbar action. */
export interface ToolbarActionOutcome {
  /** The command dispatched, or `null` for a local-only action. */
  command: Command | null;
  /** The correlation `opId` of a mutation, registered as an optimistic op. */
  opId: Ulid | null;
}

/** Hooks `runToolbarAction` uses to drive optimistic UI (the store actions). */
export interface OptimisticHooks {
  beginOptimistic: (opId: Ulid, label: string, subjectId?: Ulid | null) => void;
}

/**
 * Extract the freshly-minted subject id from a `MUTATION` result's `ids` bag —
 * the new `nodeId`, or the `refId` for ref ops (DESIGN.md A.1). This is the id a
 * tailing op-log event will reference, so the store can reconcile the optimistic
 * op against it (the single reconciliation path). `null` when the bag carries no
 * addressable subject.
 */
function mintedSubjectId(ids: Record<string, unknown>): Ulid | null {
  const node = ids["nodeId"];
  if (typeof node === "string") return node;
  const ref = ids["refId"];
  if (typeof ref === "string") return ref;
  return null;
}

/**
 * Run a toolbar action: build its command, dispatch it, and register the
 * returned `opId` as an optimistic op so the panel reflects the in-flight
 * mutation immediately. The resulting graph state arrives over the op-log stream
 * (DESIGN.md §14.4) and the store's `ingestEvent` folds it; `resolveOptimistic`
 * clears the pending op once its event is observed — the single reconciliation
 * path.
 *
 * A read-only / local-only action (no `buildCommand` or a `null` command) is a
 * no-op against the daemon: it returns immediately with no `opId`.
 */
export async function runToolbarAction(
  action: ToolbarAction,
  node: NodeView | null,
  ctx: ToolbarContext,
  hooks: OptimisticHooks,
): Promise<ToolbarActionOutcome> {
  const command = action.buildCommand ? action.buildCommand(node, ctx) : null;
  if (command === null) return { command: null, opId: null };

  const res = await dispatch(command);
  if (res.result === "MUTATION") {
    hooks.beginOptimistic(res.opId, action.label, mintedSubjectId(res.ids));
    return { command, opId: res.opId };
  }
  // A non-mutation reply (a read action wired through here) carries no opId.
  return { command, opId: null };
}
