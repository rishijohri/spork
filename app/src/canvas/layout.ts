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

/** A horizontal **swimlane** band for one emergent line (REALIGNMENT_PLAN §5a). */
export interface LaneBand {
  /** The internal branch id this lane represents (never shown raw). */
  branchId: string;
  /** The friendly line label drawn in the gutter. */
  label: string;
  /** The lane's row index (main = 0). */
  index: number;
  /** The band's top edge in flow coordinates. */
  yTop: number;
  /** The band's height. */
  height: number;
  /** Where this line forked from (drives the ↳ gutter mark), or null. */
  forkedFrom: string | null;
}

/** A lane-aware layout: positions grouped into per-line swimlanes + the bands. */
export interface LaneLayout {
  positions: NodePosition[];
  lanes: LaneBand[];
  /** The total band width spanning every depth column. */
  width: number;
}

const COL_W = NODE_W + 110;
const SUBROW_H = NODE_H + 18;
const LANE_PAD = 26;

/**
 * Lay nodes out in **swimlanes** — one horizontal band per emergent line
 * (`branchId`), X by lineage depth — so a fork visibly drops into a new lane
 * (REALIGNMENT_PLAN §5a). `lines` fixes the lane order (main first); any
 * branchId not in `lines` is appended defensively. Deterministic + dependency-
 * free, so the canvas mounts with stable lane positions immediately.
 */
export function laneLayout(
  nodes: readonly NodeView[],
  lines: readonly { branchId: string; label: string; forkedFrom: string | null }[],
): LaneLayout {
  const laneIndex = new Map<string, number>();
  lines.forEach((l) => laneIndex.set(l.branchId, laneIndex.size));
  for (const n of nodes) {
    if (!laneIndex.has(n.branchId)) laneIndex.set(n.branchId, laneIndex.size);
  }

  const depth = new Map<string, number>();
  for (const n of nodes) {
    depth.set(
      n.id,
      n.parentIds.reduce((max, p) => Math.max(max, (depth.get(p) ?? 0) + 1), 0),
    );
  }

  // Allocate a within-lane sub-row per node so two nodes at the same depth in the
  // same lane don't overlap (e.g. an observing check beside its edit).
  const subRow = new Map<string, number>();
  const cellCount = new Map<string, number>();
  const laneRows = new Map<number, number>();
  for (const n of nodes) {
    const lane = laneIndex.get(n.branchId) ?? 0;
    const d = depth.get(n.id) ?? 0;
    const key = `${lane}:${d}`;
    const c = cellCount.get(key) ?? 0;
    subRow.set(n.id, c);
    cellCount.set(key, c + 1);
    laneRows.set(lane, Math.max(laneRows.get(lane) ?? 1, c + 1));
  }

  const laneCount = laneIndex.size;
  const laneYTop = new Map<number, number>();
  let y = 0;
  for (let i = 0; i < laneCount; i++) {
    laneYTop.set(i, y);
    y += (laneRows.get(i) ?? 1) * SUBROW_H + LANE_PAD * 2;
  }

  const positions: NodePosition[] = nodes.map((n) => {
    const lane = laneIndex.get(n.branchId) ?? 0;
    const d = depth.get(n.id) ?? 0;
    const sr = subRow.get(n.id) ?? 0;
    return {
      id: n.id,
      x: d * COL_W + 56,
      y: (laneYTop.get(lane) ?? 0) + LANE_PAD + sr * SUBROW_H,
    };
  });

  const maxDepth = nodes.length
    ? Math.max(...Array.from(depth.values()))
    : 0;
  const width = (maxDepth + 1) * COL_W + 80;

  const lanes: LaneBand[] = Array.from(laneIndex.entries())
    .map(([branchId, index]) => {
      const line = lines.find((l) => l.branchId === branchId);
      return {
        branchId,
        label: line?.label ?? branchId,
        index,
        yTop: laneYTop.get(index) ?? 0,
        height: (laneRows.get(index) ?? 1) * SUBROW_H + LANE_PAD * 2,
        forkedFrom: line?.forkedFrom ?? null,
      };
    })
    .sort((a, b) => a.index - b.index);

  return { positions, lanes, width };
}
