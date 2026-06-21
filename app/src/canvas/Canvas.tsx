// The DAG canvas (UI_UX_DESIGN.md §5.4, §14.1, §14.3, §14.5).
//
// Binds the view-model to React Flow as a left-to-right typed-node graph of rich,
// schema-driven NodeCards. Edges are color-coded by type and humanized (never raw
// SCREAMING_SNAKE). The canvas has its own controls (zoom / fit / minimap),
// a free-text search that dims non-matching nodes, a node-type filter (driven by
// the navigator legend), a right-click context menu, and an honest empty state.
//
// Selection is local (store); selecting a node drives the Node-Details Changes
// tab's lazy diff (DESIGN §14.5 — selection is O(changed files)). Positions come
// from the deterministic L->R fallback layout so the canvas always mounts.

import { useCallback, useMemo, useState, type JSX } from "react";
import {
  ReactFlow,
  Background,
  BackgroundVariant,
  MiniMap,
  useReactFlow,
  type Node as RfNode,
  type Edge as RfEdge,
  type NodeMouseHandler,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useUiStore } from "../state/store";
import { descriptorFor, type NodeTypeDescriptor } from "./descriptors";
import { fallbackLayout } from "./layout";
import { NodeCard, type NodeCardData } from "./NodeCard";
import { Icon } from "../ui/icons";
import { IconButton } from "../ui/Button";
import { humanizeEdge } from "../ui/format";
import { openProject } from "../ipc/client";
import { GRAPH_VIEW_KEY } from "../state/queries";
import { useQueryClient } from "@tanstack/react-query";
import type { GraphView, NodeView } from "../ipc/types";

const NODE_TYPES: NodeTypes = { sporkCard: NodeCard };

/** Whether a node matches the free-text canvas search (kind/label/id/branch/model). */
function matchesSearch(
  node: NodeView,
  descriptor: NodeTypeDescriptor,
  q: string,
): boolean {
  if (!q) return true;
  const hay = [
    node.kind,
    descriptor.label,
    node.id,
    node.branchId,
    node.model ?? "",
  ]
    .join(" ")
    .toLowerCase();
  return hay.includes(q.toLowerCase());
}

/** Map the view-model into React Flow nodes (custom cards, positioned, dimmed). */
export function toRfNodes(
  view: GraphView,
  selectedId: string | null,
  kindFilter: readonly string[],
  search: string,
): RfNode<NodeCardData>[] {
  const positions = new Map(fallbackLayout(view.nodes).map((p) => [p.id, p]));
  return view.nodes.map((n) => {
    const descriptor = descriptorFor(n.kind);
    const pos = positions.get(n.id) ?? { x: 0, y: 0 };
    const filteredOut =
      kindFilter.length > 0 && !kindFilter.includes(n.kind);
    const searchedOut = !matchesSearch(n, descriptor, search);
    return {
      id: n.id,
      type: "sporkCard",
      position: { x: pos.x, y: pos.y },
      data: {
        node: n,
        descriptor,
        selected: n.id === selectedId,
        dimmed: filteredOut || searchedOut,
      },
      // a stable className for tests/inspection
      className: `spork-node spork-node-${n.kind}`,
    } satisfies RfNode<NodeCardData>;
  });
}

/** Map the view-model edges into React Flow edges (humanized + color-coded). */
export function toRfEdges(view: GraphView): RfEdge[] {
  return view.edges.map((e, i) => ({
    id: `e${i}-${e.from}-${e.to}`,
    source: e.from,
    target: e.to,
    // Only non-lineage edges carry a visible label (parent links are implicit),
    // and it is humanized — never the raw enum tag.
    label: e.edgeType === "PARENT_CHILD" ? undefined : humanizeEdge(e.edgeType),
    className: `spork-edge spork-edge-${e.edgeType}`,
  }));
}

/** The center DAG canvas region. */
export function Canvas(): JSX.Element {
  const view = useUiStore((s) => s.view);
  const selectedNodeId = useUiStore((s) => s.selectedNodeId);
  const selectNode = useUiStore((s) => s.selectNode);
  const kindFilter = useUiStore((s) => s.kindFilter);
  const canvasSearch = useUiStore((s) => s.canvasSearch);
  const setCanvasSearch = useUiStore((s) => s.setCanvasSearch);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const closeContextMenu = useUiStore((s) => s.closeContextMenu);

  const nodes = useMemo(
    () => toRfNodes(view, selectedNodeId, kindFilter, canvasSearch),
    [view, selectedNodeId, kindFilter, canvasSearch],
  );
  const edges = useMemo(() => toRfEdges(view), [view]);

  const onNodeClick = useCallback<NodeMouseHandler>(
    (_e, node) => selectNode(node.id),
    [selectNode],
  );

  const onNodeContextMenu = useCallback<NodeMouseHandler>(
    (e, node) => {
      e.preventDefault();
      selectNode(node.id);
      openContextMenu({ kind: "node", nodeId: node.id, x: e.clientX, y: e.clientY });
    },
    [openContextMenu, selectNode],
  );

  if (view.nodes.length === 0) {
    return <EmptyCanvas />;
  }

  return (
    <div className="spork-canvas-wrap" data-testid="canvas">
      <div className="spork-canvas-search">
        <div className="spork-search" style={{ width: "100%" }}>
          <Icon name="search" size={14} />
          <input
            type="search"
            value={canvasSearch}
            onChange={(e) => setCanvasSearch(e.target.value)}
            placeholder="Search nodes…"
            aria-label="Search nodes"
            style={{ flex: 1, background: "transparent", border: "none", padding: 0 }}
          />
        </div>
      </div>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        onNodeClick={onNodeClick}
        onNodeContextMenu={onNodeContextMenu}
        onPaneClick={() => closeContextMenu()}
        nodesDraggable={false}
        nodesConnectable={false}
        fitView
        minZoom={0.2}
        maxZoom={2}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="#1f2735" />
        <CanvasControls />
      </ReactFlow>
    </div>
  );
}

/** Zoom / fit / minimap controls, drawn over the canvas (§5.4). */
function CanvasControls(): JSX.Element {
  const { zoomIn, zoomOut, fitView } = useReactFlow();
  const [showMap, setShowMap] = useState(true);
  return (
    <>
      {showMap && (
        <MiniMap
          className="spork-minimap"
          pannable
          zoomable
          maskColor="rgba(10,12,18,0.6)"
          nodeColor={(n) => {
            const data = n.data as NodeCardData | undefined;
            return data?.descriptor.color ?? "#5e677a";
          }}
          style={{ background: "var(--bg-surface)" }}
        />
      )}
      <div className="spork-canvas-controls" role="toolbar" aria-label="Canvas controls">
        <IconButton icon="plus" label="Zoom in" onClick={() => void zoomIn()} />
        <IconButton icon="minus" label="Zoom out" onClick={() => void zoomOut()} />
        <IconButton icon="maximize" label="Fit to view" onClick={() => void fitView()} />
        <IconButton
          icon="map"
          label={showMap ? "Hide minimap" : "Show minimap"}
          active={showMap}
          onClick={() => setShowMap((v) => !v)}
        />
      </div>
    </>
  );
}

/**
 * Honest empty state (§5.4, adjusted for what's actually possible): a project's
 * nodes are produced by the daemon (drift capture as you edit, agent runs), not
 * fabricated by the renderer — creating a root snapshot needs a real content hash
 * the UI cannot mint. So the empty state explains that and offers to open a
 * project (the real, backed `open_project` path), rather than a dead "New node".
 */
function EmptyCanvas(): JSX.Element {
  const qc = useQueryClient();
  const logActivity = useUiStore((s) => s.logActivity);
  const [path, setPath] = useState("");

  async function open(): Promise<void> {
    const p = path.trim();
    if (!p) return;
    try {
      await openProject(p);
      await qc.invalidateQueries({ queryKey: GRAPH_VIEW_KEY });
      logActivity("success", `Opened project ${p}`);
    } catch (err) {
      logActivity("error", `Open failed: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  return (
    <div className="spork-canvas-wrap" data-testid="canvas-empty">
      <div className="spork-empty">
        <div className="spork-empty-icon">
          <Icon name="git-branch" size={22} />
        </div>
        <h3>No work yet</h3>
        <p className="spork-muted" style={{ maxWidth: 340 }}>
          Spork captures work as a graph — nodes appear as the daemon records your
          edits (drift capture) and agent runs. Open a project to begin.
        </p>
        <div className="spork-empty-actions">
          <input
            type="text"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/path/to/project"
            aria-label="Project path"
            style={{ width: 220 }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void open();
            }}
          />
          <button className="btn btn--primary" onClick={() => void open()}>
            <Icon name="download" size={14} /> Open project
          </button>
        </div>
      </div>
    </div>
  );
}
