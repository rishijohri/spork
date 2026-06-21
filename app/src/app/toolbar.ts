// Node-action toolbar metadata + dispatch helpers (UI_UX_DESIGN.md §5.10, §7.1).
//
// Spork is a general codebase-editing IDE over a content-addressed work-DAG, so
// the toolbar's actions operate on the SELECTED node of that DAG. The v1 (🟢)
// action set is exactly the operations backed by a built IPC command:
//
//   Restore      → NODE_RESTORE   (confirm modal; working-tree + conversation)
//   Run check ▾  → NODE_RUN_CHECK (Validate / Stress / Sanity submenu)
//   New Branch   → BRANCH_FORK    (name modal)
//   Merge…       → BRANCH_MERGE   (from/into modal, 3-way)
//   Commit       → GIT_EXPORT     (branch modal)
//   Push         → GIT_PUSH       (remote modal; net.connect-gated)
//
// The vague View / Analyze / Metadata buttons of the first cut are gone (no
// backing command; "Metadata" became the Info tab). Each action declares
// `enabledWhen(node)` for schema-driven gating (§14.5). Most actions OPEN A MODAL
// (so a name/remote/confirm is collected); the modal does the dispatch via the
// shared action runner (useActions.ts). Run-check dispatches from a submenu.

import type { Command, CommandResult, NodeView, Ulid } from "../ipc/types";
import type { ActivityLevel } from "../state/store";
import type { IconName } from "../ui/icons";
import type { ButtonVariant } from "../ui/Button";
import { shortId } from "../ui/format";

/** The v1 node-action ids. */
export type ToolbarActionId =
  | "restore"
  | "runCheck"
  | "newBranch"
  | "merge"
  | "commit"
  | "push";

/** A node-action's display + gating metadata. */
export interface ToolbarAction {
  id: ToolbarActionId;
  label: string;
  icon: IconName;
  variant?: ButtonVariant;
  /** Whether the action is enabled for the given selection (null = none). */
  enabledWhen: (node: NodeView | null) => boolean;
}

/** True iff a node exists and owns a restorable snapshot. */
export function isMaterializable(node: NodeView | null): node is NodeView {
  return node !== null && node.ownsSnapshot;
}

/** True iff a node exists at all (any selection). */
export function hasSelection(node: NodeView | null): node is NodeView {
  return node !== null;
}

/** The frozen v1 node-action set, in display order. */
export const TOOLBAR_ACTIONS: readonly ToolbarAction[] = [
  {
    id: "restore",
    label: "Restore",
    icon: "corner-up-left",
    variant: "danger",
    enabledWhen: (n) => isMaterializable(n) && n.family === "mutating",
  },
  {
    id: "runCheck",
    label: "Run check",
    icon: "play",
    enabledWhen: isMaterializable,
  },
  {
    id: "newBranch",
    label: "Branch",
    icon: "git-branch",
    enabledWhen: hasSelection,
  },
  {
    id: "merge",
    label: "Merge",
    icon: "git-merge",
    enabledWhen: hasSelection,
  },
  {
    id: "commit",
    label: "Commit",
    icon: "git-commit",
    enabledWhen: isMaterializable,
  },
  {
    id: "push",
    label: "Push",
    icon: "upload",
    variant: "danger",
    enabledWhen: isMaterializable,
  },
];

/** The check kinds the Run-check submenu offers (all P5 runners). */
export const CHECK_KINDS = [
  { kind: "validation", label: "Validate" },
  { kind: "stress", label: "Stress" },
  { kind: "sanity", label: "Sanity" },
] as const;

/** Look up an action by id. */
export function toolbarActionById(id: string): ToolbarAction | undefined {
  return TOOLBAR_ACTIONS.find((a) => a.id === id);
}

/** The check kind a NODE_RUN_CHECK spec carries (opaque `unknown` on the command). */
function checkKind(spec: unknown): string {
  if (spec !== null && typeof spec === "object") {
    const k = (spec as Record<string, unknown>)["kind"];
    if (typeof k === "string") return k;
  }
  return "validation";
}

/**
 * Build a human-readable activity line for a settled command + reply. Returns
 * null when the reply warrants no line (it never silently drops a mutation
 * outcome). `nodeId` is the acted-on node, for the short-id reference.
 */
export function activityLineFor(
  command: Command,
  res: CommandResult,
  nodeId?: Ulid | null,
): { level: ActivityLevel; text: string } | null {
  const id = shortId(nodeId);
  switch (command.command) {
    case "GIT_EXPORT":
      return res.result === "GIT"
        ? {
            level: "success",
            text: `Committed ${id} → ${res.branch} @ ${res.commitSha.slice(0, 8)}`,
          }
        : null;
    case "GIT_PUSH":
      return res.result === "GIT"
        ? {
            level: "success",
            text: `Pushed ${res.branch} → ${command.remote ?? "origin"}`,
          }
        : null;
    case "BRANCH_FORK":
      return { level: "success", text: `Created branch ${command.name}` };
    case "NODE_RESTORE":
      return { level: "success", text: `Restored ${id} (code + conversation)` };
    case "NODE_RUN_CHECK":
      return {
        level: "info",
        text: `Queued ${checkKind(command.spec)} check on ${id}`,
      };
    case "BRANCH_MERGE":
      return { level: "success", text: `Merged into ${command.intoRef}` };
    case "REF_CREATE":
      return { level: "success", text: `Created ref ${command.name}` };
    case "REF_MOVE":
      return { level: "success", text: `Moved ${command.name} → ${shortId(command.to)}` };
    case "GC_RUN":
      return res.result === "GC"
        ? {
            level: "success",
            text: command.dryRun
              ? `GC dry-run: ${res.reclaimable.length} objects · ${res.bytes} bytes reclaimable`
              : `GC freed ${res.reclaimable.length} objects · ${res.bytes} bytes`,
          }
        : null;
    case "OP_UNDO":
      return { level: "info", text: "Undid last operation" };
    case "OP_REDO":
      return { level: "info", text: "Redid operation" };
    case "NODE_AGENT_RUN": {
      if (res.result !== "MUTATION") return null;
      const model =
        typeof res.ids["model"] === "string" ? (res.ids["model"] as string) : command.modelKey;
      const micro =
        typeof res.ids["costMicroUsd"] === "number" ? (res.ids["costMicroUsd"] as number) : 0;
      const cost = micro <= 0 ? "free" : `$${(micro / 1_000_000).toFixed(4)}`;
      return { level: "success", text: `Asked agent on ${id} via ${model} · ${cost}` };
    }
    default:
      return res.result === "MUTATION"
        ? { level: "success", text: "Done" }
        : null;
  }
}

/** The minted subject id (nodeId/refId) from a MUTATION reply, for reconciliation. */
export function mintedSubjectId(ids: Record<string, unknown>): Ulid | null {
  if (typeof ids["nodeId"] === "string") return ids["nodeId"] as Ulid;
  if (typeof ids["refId"] === "string") return ids["refId"] as Ulid;
  return null;
}

/** The capability a denied command most likely needs (for the denial modal). */
export function capabilityForCommand(command: Command): string {
  switch (command.command) {
    case "GIT_PUSH":
      return "net.connect";
    case "GIT_EXPORT":
      return "snapshot.read";
    case "NODE_CREATE":
    case "NODE_RESTORE":
    case "BRANCH_MERGE":
      return "snapshot.write";
    case "NODE_RUN_CHECK":
      return "process.spawn";
    case "NODE_AGENT_RUN":
      return "model.invoke";
    default:
      return "capability";
  }
}

/** Heuristic: does a dispatch error look like a capability denial? */
export function isCapabilityDenial(message: string): boolean {
  return /capab|denied|permission|not granted|net\.connect/i.test(message);
}
