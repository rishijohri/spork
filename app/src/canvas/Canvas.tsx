// The DAG canvas (DESIGN.md §14.2, §14.3, §14.5).
//
// Binds the view-model to React Flow as a left-to-right typed-node graph. Node
// cards are schema-driven (color/label/icon come from the node-type descriptor
// resolved by `kind`), so built-in and custom types render identically. Clicking
// a node sets the selection in the store and triggers a lazy NodeDiff fetch
// (DESIGN.md §14.5 — selection is O(changed files)).
//
// Layout positions come from the L->R fallback layout synchronously so the
// canvas always mounts; the ELK worker layout (src/canvas/layout.ts) refines
// them. ResizeObserver is mocked in tests (src/test/setup.ts).

import { useMemo, useCallback } from "react";
import {
  ReactFlow,
  Background,
  type Node as RfNode,
  type Edge as RfEdge,
  type NodeMouseHandler,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useUiStore } from "../state/store";
import { descriptorFor } from "./descriptors";
import { fallbackLayout } from "./layout";
import type { GraphView } from "../ipc/types";

/** Map the view-model into React Flow nodes, positioned + descriptor-styled. */
export function toRfNodes(view: GraphView, selectedId: string | null): RfNode[] {
  const positions = new Map(fallbackLayout(view.nodes).map((p) => [p.id, p]));
  return view.nodes.map((n) => {
    const d = descriptorFor(n.kind);
    const pos = positions.get(n.id) ?? { x: 0, y: 0 };
    return {
      id: n.id,
      position: { x: pos.x, y: pos.y },
      data: { label: `${d.icon} ${d.label}` },
      style: {
        borderColor: d.color,
        borderWidth: n.id === selectedId ? 3 : 1,
        borderStyle: "solid",
        borderRadius: 8,
        padding: 8,
        background: "var(--card-bg, #1e1e1e)",
        color: "var(--card-fg, #e5e7eb)",
        width: 180,
        opacity: n.isStale ? 0.6 : 1,
      },
      // expose the kind for tests/inspection
      type: "default",
      className: `spork-node spork-node-${n.kind}`,
    } satisfies RfNode;
  });
}

/** Map the view-model edges into React Flow edges. */
export function toRfEdges(view: GraphView): RfEdge[] {
  return view.edges.map((e, i) => ({
    id: `e${i}-${e.from}-${e.to}`,
    source: e.from,
    target: e.to,
    label: e.edgeType,
    className: `spork-edge spork-edge-${e.edgeType}`,
  }));
}

export interface CanvasProps {
  /** Called when a node is selected (id), so the host can trigger a diff fetch. */
  onSelect?: (nodeId: string) => void;
}

/** The center DAG canvas region. */
export function Canvas({ onSelect }: CanvasProps): JSX.Element {
  const view = useUiStore((s) => s.view);
  const selectedNodeId = useUiStore((s) => s.selectedNodeId);
  const selectNode = useUiStore((s) => s.selectNode);

  const nodes = useMemo(
    () => toRfNodes(view, selectedNodeId),
    [view, selectedNodeId],
  );
  const edges = useMemo(() => toRfEdges(view), [view]);

  const onNodeClick = useCallback<NodeMouseHandler>(
    (_event, node) => {
      selectNode(node.id);
      onSelect?.(node.id);
    },
    [selectNode, onSelect],
  );

  return (
    <div className="spork-canvas" style={{ width: "100%", height: "100%" }}>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodeClick={onNodeClick}
        fitView
        proOptions={{ hideAttribution: true }}
      >
        <Background />
      </ReactFlow>
    </div>
  );
}
