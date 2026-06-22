// Node-action toolbar metadata + dispatch helpers (UI_UX_DESIGN.md §5.10, §7.1;
// REALIGNMENT_PLAN §5a).
//
// Spork is the timeline-first layer over a content-addressed work-DAG, so the
// toolbar acts on the SELECTED node. Branching is **automatic** (fork-on-
// divergence) — there is no "new branch" verb. The actions are reframed into:
//
//   "+ New node from here ▾"  — create a node from the selection, in two groups:
//      Agentic: Ask / Plan / Explore / Work  (→ NODE_AGENT_RUN / NODE_AGENT_EDIT)
//      Action:  Run tests / Stress / Sanity  (→ NODE_RUN_CHECK)
//               Commit / Push                (→ GIT_EXPORT / GIT_PUSH)
//   Restore           — NODE_RESTORE (confirm modal; working-tree + conversation)
//   Merge into line   — BRANCH_MERGE (pick a destination *line tip*, 3-way)
//
// Each item declares `enabledWhen(node)` for schema-driven gating (§14.5). An
// agentic item opens the Ask modal pre-set to its intent; an action check item
// dispatches directly; commit/push open their modals; the modals do the dispatch
// via the shared action runner (useActions.ts).

import type {
  AgentRunIntent,
  Command,
  CommandResult,
  NodeView,
  Ulid,
} from "../ipc/types";
import type { ActivityLevel } from "../state/store";
import type { IconName } from "../ui/icons";
import type { ButtonVariant } from "../ui/Button";
import { shortId } from "../ui/format";

/** True iff a node exists and owns a restorable snapshot. */
export function isMaterializable(node: NodeView | null): node is NodeView {
  return node !== null && node.ownsSnapshot;
}

/** True iff a node exists at all (any selection). */
export function hasSelection(node: NodeView | null): node is NodeView {
  return node !== null;
}

/** A direct node-operation on the selection (not a node-creation). */
export type NodeOpId = "restore" | "merge";

/** A node-op's display + gating metadata. */
export interface NodeOp {
  id: NodeOpId;
  label: string;
  icon: IconName;
  variant?: ButtonVariant;
  enabledWhen: (node: NodeView | null) => boolean;
}

/** The direct node-ops, in display order (alongside the "+ New node" menu). */
export const NODE_OPS: readonly NodeOp[] = [
  {
    id: "restore",
    label: "Restore",
    icon: "corner-up-left",
    variant: "danger",
    enabledWhen: (n) => isMaterializable(n) && n.family === "mutating",
  },
  {
    id: "merge",
    label: "Merge into line",
    icon: "git-merge",
    enabledWhen: hasSelection,
  },
];

/** What a "+ New node from here" item dispatches when chosen. */
export type NewNodeDispatch =
  /** Open the Ask modal pre-set to an agentic intent (ask/plan/analysis/change). */
  | { type: "askAgent"; intent: AgentRunIntent }
  /** Dispatch a deterministic check directly (validation/stress/sanity). */
  | { type: "check"; checkKind: string }
  /** Open a git action modal (commit/push). */
  | { type: "modal"; modal: "commit" | "push" };

/** One item in the "+ New node from here" menu. */
export interface NewNodeItem {
  id: string;
  label: string;
  icon: IconName;
  /** A one-line description shown under the item. */
  help: string;
  dispatch: NewNodeDispatch;
  enabledWhen: (node: NodeView | null) => boolean;
}

/** A titled group of new-node items (Agentic / Action). */
export interface NewNodeGroup {
  label: string;
  items: readonly NewNodeItem[];
}

/** The "+ New node from here" menu: the agentic + deterministic-action families. */
export const NEW_NODE_GROUPS: readonly NewNodeGroup[] = [
  {
    label: "Agentic",
    items: [
      {
        id: "ask",
        label: "Ask",
        icon: "messages-square",
        help: "Ask a question about this node",
        dispatch: { type: "askAgent", intent: "ask" },
        enabledWhen: hasSelection,
      },
      {
        id: "plan",
        label: "Plan",
        icon: "list",
        help: "Plan a change — nothing is edited",
        dispatch: { type: "askAgent", intent: "plan" },
        enabledWhen: hasSelection,
      },
      {
        id: "explore",
        label: "Explore",
        icon: "search",
        help: "Analyze, review, or summarize",
        dispatch: { type: "askAgent", intent: "analysis" },
        enabledWhen: hasSelection,
      },
      {
        id: "work",
        label: "Work",
        icon: "pencil",
        help: "Make a change — creates an Edit node (auto-forks)",
        dispatch: { type: "askAgent", intent: "change" },
        enabledWhen: isMaterializable,
      },
    ],
  },
  {
    label: "Action",
    items: [
      {
        id: "run-tests",
        label: "Run tests",
        icon: "check-circle",
        help: "Run the validation suite",
        dispatch: { type: "check", checkKind: "validation" },
        enabledWhen: isMaterializable,
      },
      {
        id: "stress",
        label: "Stress",
        icon: "activity",
        help: "Run a stress check",
        dispatch: { type: "check", checkKind: "stress" },
        enabledWhen: isMaterializable,
      },
      {
        id: "sanity",
        label: "Sanity",
        icon: "shield-check",
        help: "Run a sanity check",
        dispatch: { type: "check", checkKind: "sanity" },
        enabledWhen: isMaterializable,
      },
      {
        id: "commit",
        label: "Commit",
        icon: "git-commit",
        help: "Commit this node's state to git",
        dispatch: { type: "modal", modal: "commit" },
        enabledWhen: isMaterializable,
      },
      {
        id: "push",
        label: "Push",
        icon: "upload",
        help: "Push a line to a remote",
        dispatch: { type: "modal", modal: "push" },
        enabledWhen: isMaterializable,
      },
    ],
  },
];

/** The check kinds (kept for the command palette / external callers). */
export const CHECK_KINDS = [
  { kind: "validation", label: "Validate" },
  { kind: "stress", label: "Stress" },
  { kind: "sanity", label: "Sanity" },
] as const;

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
      return { level: "success", text: `Started a new line ${command.name}` };
    case "NODE_RESTORE":
      return { level: "success", text: `Restored ${id} (code + conversation)` };
    case "NODE_RUN_CHECK":
      return {
        level: "info",
        text: `Queued ${checkKind(command.spec)} check on ${id}`,
      };
    case "BRANCH_MERGE":
      return { level: "success", text: `Merged into line ${command.intoRef}` };
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
    case "NODE_AGENT_EDIT": {
      if (res.result !== "MUTATION") return null;
      const line =
        typeof res.ids["branchId"] === "string" ? (res.ids["branchId"] as string) : "main";
      const forked = res.ids["forked"] === true ? " (new line)" : "";
      return { level: "success", text: `Agent edited ${id} → ${line}${forked}` };
    }
    default:
      return res.result === "MUTATION"
        ? { level: "success", text: "Done" }
        : null;
  }
}

/** The minted subject id (nodeId/refId/editNodeId) from a MUTATION reply. */
export function mintedSubjectId(ids: Record<string, unknown>): Ulid | null {
  if (typeof ids["nodeId"] === "string") return ids["nodeId"] as Ulid;
  if (typeof ids["editNodeId"] === "string") return ids["editNodeId"] as Ulid;
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
    case "NODE_AGENT_EDIT":
      return "model.invoke";
    default:
      return "capability";
  }
}

/** Heuristic: does a dispatch error look like a capability denial? */
export function isCapabilityDenial(message: string): boolean {
  return /capab|denied|permission|not granted|net\.connect/i.test(message);
}
