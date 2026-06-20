// Top-bar action toolbar (DESIGN.md §14.2, §14.5).
//
// The reference UI's toolbar: View, Analyze, Recalibrate, Validate, Create DT,
// Submit DT, Create Process, Metadata. Each action declares `enabledWhen(node)`
// so a node exposes only the actions valid for its kind/family — exactly the
// schema-driven gating §14.5 requires. With no selection, only the
// always-available actions (e.g. Create Process) are enabled.
//
// Each MUTATING action also declares `buildCommand(node, ctx)`: the frozen
// `spork_ipc::Command` it dispatches (A.1, src/ipc/types.ts). `runToolbarAction`
// is the single wired path the panels call — it dispatches the command, registers
// the returned `opId` as an optimistic op, and lets the op-log stream reconcile it
// (the store's `ingestEvent` + `resolveOptimistic`, DESIGN.md §14.4). Read-only
// actions (View / Analyze / Metadata) carry no command and are local-only UI
// selections, so they never touch the daemon.

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
  // Recalibrate — only meaningful on a mutating, snapshot-owning node. Restores
  // the node's snapshot+conversation (A.1 `node.restore`) as the recalibration
  // baseline (DESIGN.md §6.4 — restore is an event, never an overwrite).
  {
    id: "recalibrate",
    label: "Recalibrate",
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
  // Create DT (decision/diff transaction) — fork a working branch off the
  // selected node to stage the transaction (A.1 `branch.fork`).
  {
    id: "createDt",
    label: "Create DT",
    enabledWhen: hasSelection,
    buildCommand: (n) =>
      n ? { command: "BRANCH_FORK", fromNodeId: n.id, name: `dt/${n.id}` } : null,
  },
  // Submit DT — merge the DT branch back into the selected node's branch
  // (A.1 `branch.merge`; a clean merge or a conflictSet, DESIGN.md §6.6).
  {
    id: "submitDt",
    label: "Submit DT",
    enabledWhen: hasSelection,
    buildCommand: (n) =>
      n
        ? {
            command: "BRANCH_MERGE",
            intoRef: n.branchId,
            fromNodeId: n.id,
            resolution: null,
          }
        : null,
  },
  // Create Process — always available: start a new root flow with a fresh
  // snapshot node on `main`, authored against the top-bar default model
  // (A.1 `node.create`). The model rides the payload (the registry validates it).
  {
    id: "createProcess",
    label: "Create Process",
    enabledWhen: () => true,
    buildCommand: (n, ctx) => ({
      command: "NODE_CREATE",
      kind: "snapshot",
      typeVersion: "1.0.0",
      parentIds: n ? [n.id] : [],
      branchId: n?.branchId ?? "main",
      payload: { origin: "manual", model: ctx.defaultModel },
      ownsSnapshot: false,
      snapshotHash: null,
    }),
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
