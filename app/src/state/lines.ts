// Emergent **lines** (lanes) derived from the node DAG (REALIGNMENT_PLAN §1, §5a).
//
// A "line" is *not* a git branch the user manages — it is the emergent line a
// node's internal `branchId` denotes. The de-git-ified UI groups nodes into lanes
// by `branchId` and presents each as a line with a friendly label, a tip, a node
// count, and a rolled-up state. The daemon already provides the friendly
// `lineLabel` and the fork origin `forkedFrom` per node (R2 additive view fields);
// this module folds the view-model into the per-line shape the Navigator,
// canvas swimlanes, and hero chat all consume — one derivation, no duplication.

import type { GraphView, Lifecycle, NodeView, Ulid } from "../ipc/types";

/** A rolled-up presentation state for a whole line (worst-state-wins). */
export type LineRollup =
  | "failed"
  | "blocked"
  | "running"
  | "pending"
  | "passed";

/** One emergent line (lane) in the timeline. */
export interface Line {
  /** The internal branch id that denotes this line (never shown raw to users). */
  branchId: string;
  /** The friendly label (the daemon's `lineLabel`, falling back to the id). */
  label: string;
  /** Every node on the line, in creation order (ULID-sorted). */
  nodeIds: Ulid[];
  /** The line's tip — its most recent node. */
  tipNodeId: Ulid;
  /** How many nodes the line carries. */
  count: number;
  /** The rolled-up state badge for the lane (worst-wins across its nodes). */
  rollup: LineRollup;
  /** Where this line forked from (the divergence node), or null for the root line. */
  forkedFrom: Ulid | null;
}

/** Map one node's lifecycle (+ staleness) onto a line-rollup token. */
function nodeRollup(node: NodeView): LineRollup {
  switch (node.status as Lifecycle) {
    case "failed":
    case "cancelled":
      return "failed";
    case "blocked":
      return "blocked";
    case "running":
      return "running";
    case "pending":
      return "pending";
    default:
      // `passed` (and any future terminal-ok state) is the quiet baseline.
      return "passed";
  }
}

/** Severity order for the worst-state-wins fold (higher = louder). */
const ROLLUP_RANK: Record<LineRollup, number> = {
  passed: 0,
  pending: 1,
  running: 2,
  blocked: 3,
  failed: 4,
};

/**
 * Derive the emergent lines from a view-model, ordered with `main` first and the
 * rest by their first node's id (creation order), so lane assignment is stable.
 */
export function deriveLines(view: GraphView): Line[] {
  const byBranch = new Map<string, NodeView[]>();
  for (const node of view.nodes) {
    const bucket = byBranch.get(node.branchId);
    if (bucket) bucket.push(node);
    else byBranch.set(node.branchId, [node]);
  }

  const lines: Line[] = [];
  for (const [branchId, nodes] of byBranch) {
    // Nodes arrive id-ordered from the daemon, but sort defensively so the tip is
    // unambiguous even for an optimistic, out-of-order insert.
    const sorted = [...nodes].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const rollup = sorted.reduce<LineRollup>(
      (worst, n) => (ROLLUP_RANK[nodeRollup(n)] > ROLLUP_RANK[worst] ? nodeRollup(n) : worst),
      "passed",
    );
    const forkOrigin = sorted.map((n) => n.forkedFrom).find((f) => f != null) ?? null;
    lines.push({
      branchId,
      label: sorted.find((n) => n.lineLabel)?.lineLabel ?? branchId,
      nodeIds: sorted.map((n) => n.id),
      tipNodeId: sorted[sorted.length - 1]!.id,
      count: sorted.length,
      rollup,
      forkedFrom: forkOrigin,
    });
  }

  return lines.sort((a, b) => {
    if (a.branchId === "main") return -1;
    if (b.branchId === "main") return 1;
    const af = a.nodeIds[0]!;
    const bf = b.nodeIds[0]!;
    return af < bf ? -1 : af > bf ? 1 : 0;
  });
}

/** The line a given node belongs to, or null if the node is unknown. */
export function lineOfNode(view: GraphView, nodeId: Ulid): Line | null {
  const node = view.nodes.find((n) => n.id === nodeId);
  if (!node) return null;
  return deriveLines(view).find((l) => l.branchId === node.branchId) ?? null;
}
