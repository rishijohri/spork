// Left-to-right layered DAG layout via ELK (DESIGN.md §14.3).
//
// The view-model nodes/edges are laid out with ELK's "layered" algorithm in
// LEFT->RIGHT direction. DESIGN.md §14.3 calls for this to run in a Web Worker
// (off the main thread, incremental, positions cached by node hash); this module
// is the pure layout function the worker wraps. It is also directly callable
// (and unit-testable) without a worker.
//
// We import the bundled `elkjs` build. In a worker context the same function is
// invoked off-thread; here it stays framework-agnostic so tests can run it in
// jsdom without spinning a Worker.

import ELK, { type ElkNode } from "elkjs/lib/elk.bundled.js";
import type { EdgeView, NodeView } from "../ipc/types";

/** A laid-out position for one node. */
export interface NodePosition {
  id: string;
  x: number;
  y: number;
}

/** Layout options: a left-to-right layered Sugiyama layout. */
const LAYOUT_OPTIONS: Record<string, string> = {
  "elk.algorithm": "layered",
  "elk.direction": "RIGHT",
  "elk.layered.spacing.nodeNodeBetweenLayers": "120",
  "elk.spacing.nodeNode": "48",
};

const NODE_W = 180;
const NODE_H = 64;

/**
 * Compute L->R layered positions for the given view-model nodes/edges.
 *
 * Returns a map of node id -> position. Pure with respect to its inputs (ELK is
 * deterministic for a given graph + options), so a node-hash position cache can
 * key off the input set (DESIGN.md §14.3).
 */
export async function computeLayout(
  nodes: readonly NodeView[],
  edges: readonly EdgeView[],
): Promise<NodePosition[]> {
  if (nodes.length === 0) return [];

  const elk = new ELK();
  const graph: ElkNode = {
    id: "root",
    layoutOptions: LAYOUT_OPTIONS,
    children: nodes.map((n) => ({
      id: n.id,
      width: NODE_W,
      height: NODE_H,
    })),
    edges: edges.map((e, i) => ({
      id: `e${i}`,
      sources: [e.from],
      targets: [e.to],
    })),
  };

  const laidOut = await elk.layout(graph);
  return (laidOut.children ?? []).map((c) => ({
    id: c.id,
    x: c.x ?? 0,
    y: c.y ?? 0,
  }));
}

/**
 * A synchronous fallback layout used when a worker/ELK is unavailable (e.g. an
 * initial render frame): a simple depth-by-parent L->R staircase. Deterministic
 * and dependency-free, so the canvas always has positions to mount with.
 */
export function fallbackLayout(
  nodes: readonly NodeView[],
): NodePosition[] {
  const depth = new Map<string, number>();
  // Topological-ish depth = longest parent chain; nodes are ULID-ordered so a
  // single forward pass over parentIds suffices for a DAG.
  for (const n of nodes) {
    const d = n.parentIds.reduce(
      (max, p) => Math.max(max, (depth.get(p) ?? 0) + 1),
      0,
    );
    depth.set(n.id, d);
  }
  const rowOf = new Map<number, number>();
  return nodes.map((n) => {
    const d = depth.get(n.id) ?? 0;
    const row = rowOf.get(d) ?? 0;
    rowOf.set(d, row + 1);
    return {
      id: n.id,
      x: d * (NODE_W + 120),
      y: row * (NODE_H + 48),
    };
  });
}
