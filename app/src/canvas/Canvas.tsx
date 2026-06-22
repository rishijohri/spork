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
  ViewportPortal,
  useReactFlow,
  type Node as RfNode,
  type Edge as RfEdge,
  type NodeMouseHandler,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useUiStore } from "../state/store";
import { descriptorFor, type NodeTypeDescriptor } from "./descriptors";
import { laneLayout, type LaneBand } from "./layout";
import { deriveLines } from "../state/lines";
import { NodeCard, type NodeCardData } from "./NodeCard";
import { Icon } from "../ui/icons";
import { IconButton } from "../ui/Button";
import { humanizeEdge } from "../ui/format";
import { pickProjectDir, isTauri } from "../ipc/client";
import { openAndImport, rememberProject } from "../app/onboarding";
import { useQueryClient } from "@tanstack/react-query";
import type { GraphView, NodeView } from "../ipc/types";

const NODE_TYPES: NodeTypes = { sporkCard: NodeCard };

/** Whether a node matches the free-text canvas search (kind/label/id/line/model). */
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
    node.lineLabel ?? node.branchId,
    node.model ?? "",
  ]
    .join(" ")
    .toLowerCase();
  return hay.includes(q.toLowerCase());
}

/** Map the view-model into React Flow nodes — swimlane-positioned + the lane bands. */
export function toRfNodes(
  view: GraphView,
  selectedId: string | null,
  kindFilter: readonly string[],
  search: string,
  focusedLane: string | null,
): { rfNodes: RfNode<NodeCardData>[]; lanes: LaneBand[]; width: number } {
  const lines = deriveLines(view);
  const { positions, lanes, width } = laneLayout(view.nodes, lines);
  const posById = new Map(positions.map((p) => [p.id, p]));
  const rfNodes = view.nodes.map((n) => {
    const descriptor = descriptorFor(n.kind);
    const pos = posById.get(n.id) ?? { x: 0, y: 0 };
    const filteredOut = kindFilter.length > 0 && !kindFilter.includes(n.kind);
    const searchedOut = !matchesSearch(n, descriptor, search);
    const laneDimmed = focusedLane !== null && n.branchId !== focusedLane;
    return {
      id: n.id,
      type: "sporkCard",
      position: { x: pos.x, y: pos.y },
      data: {
        node: n,
        descriptor,
        selected: n.id === selectedId,
        dimmed: filteredOut || searchedOut || laneDimmed,
      },
      // a stable className for tests/inspection
      className: `spork-node spork-node-${n.kind}`,
    } satisfies RfNode<NodeCardData>;
  });
  return { rfNodes, lanes, width };
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
  const projectOpening = useUiStore((s) => s.projectOpening);
  const focusedLane = useUiStore((s) => s.focusedLane);
  const focusLane = useUiStore((s) => s.focusLane);

  const { rfNodes, lanes, width } = useMemo(
    () => toRfNodes(view, selectedNodeId, kindFilter, canvasSearch, focusedLane),
    [view, selectedNodeId, kindFilter, canvasSearch, focusedLane],
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
    // While a project is opening/importing, show a spinner — not the onboarding
    // picker — so a reopened/just-opened project doesn't flash the empty state
    // (and the user can't accidentally re-open mid-flight).
    return projectOpening ? <OpeningCanvas /> : <EmptyCanvas />;
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
        nodes={rfNodes}
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
        {/* Swimlane bands — one per emergent line, drawn behind the nodes in flow
            coordinates so they pan/zoom with the graph (REALIGNMENT_PLAN §5a). */}
        <ViewportPortal>
          <div className="spork-lanes-layer" aria-hidden="true">
            {lanes.map((lane) => {
              const focused = focusedLane === lane.branchId;
              const dim = focusedLane !== null && !focused;
              return (
                <div
                  key={lane.branchId}
                  className={`spork-lane-band${focused ? " spork-lane-band--focused" : ""}${dim ? " spork-lane-band--dim" : ""}`}
                  data-lane-index={lane.index % 2}
                  style={{
                    position: "absolute",
                    transform: `translate(-40px, ${lane.yTop}px)`,
                    width: width + 40,
                    height: lane.height,
                  }}
                >
                  <button
                    className="spork-lane-gutter"
                    onClick={() => focusLane(focused ? null : lane.branchId)}
                    title={`Focus the ${lane.label} line`}
                  >
                    {lane.forkedFrom && <span className="spork-lane-fork" aria-hidden="true">↳</span>}
                    {lane.label}
                  </button>
                </div>
              );
            })}
          </div>
        </ViewportPortal>
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
 * The onboarding empty state (§5.4, P7.5 MVP W1/W2): point Spork at an existing
 * repo and it captures the working tree into a **root snapshot node** so the
 * project renders on the canvas. The daemon roots at the real project dir and
 * keeps its own state under a hidden `.spork/` (git-ignored); the renderer drives
 * the real, backed `open_project` → `PROJECT_IMPORT` path (it no longer needs to
 * mint a content hash — the daemon captures the tree itself).
 */
/** A neutral spinner shown while a project is opening/importing (not the picker). */
function OpeningCanvas(): JSX.Element {
  return (
    <div className="spork-canvas-wrap" data-testid="canvas-opening">
      <div className="spork-empty">
        <div className="spork-empty-icon">
          <Icon name="loader" size={22} />
        </div>
        <h3>Opening project…</h3>
        <p className="spork-muted" style={{ maxWidth: 340 }}>
          Capturing your code into the graph. This can take a moment on a large
          repo.
        </p>
      </div>
    </div>
  );
}

function EmptyCanvas(): JSX.Element {
  const qc = useQueryClient();
  const logActivity = useUiStore((s) => s.logActivity);
  const setProjectOpening = useUiStore((s) => s.setProjectOpening);
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const native = isTauri();

  /** Onboard a project dir: open the daemon over it, then capture the first node. */
  async function onboard(p: string): Promise<void> {
    const dir = p.trim();
    if (!dir || busy) return;
    setBusy(true);
    setError(null);
    setProjectOpening(true);
    try {
      const nodeId = await openAndImport(dir, qc);
      rememberProject(dir);
      logActivity(
        "success",
        nodeId ? `Opened ${dir} — root node ${nodeId}` : `Opened project ${dir}`,
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // Surface the failure BOTH inline (always visible) and in the activity log.
      setError(`Open failed: ${message}`);
      logActivity("error", `Open failed: ${message}`);
    } finally {
      setBusy(false);
      setProjectOpening(false);
    }
  }

  /** Open the native folder picker, then onboard the chosen dir. */
  async function browse(): Promise<void> {
    if (busy) return;
    setError(null);
    try {
      const dir = await pickProjectDir();
      if (dir) {
        setPath(dir);
        await onboard(dir);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(`Folder picker failed: ${message}`);
      logActivity("error", `Folder picker failed: ${message}`);
    }
  }

  /** Primary action: in the desktop app, open the picker; else use the typed path. */
  async function primary(): Promise<void> {
    if (native && !path.trim()) {
      await browse();
    } else {
      await onboard(path);
    }
  }

  return (
    <div className="spork-canvas-wrap" data-testid="canvas-empty">
      <div className="spork-empty">
        <div className="spork-empty-icon">
          <Icon name="folder" size={22} />
        </div>
        <h3>Open a project</h3>
        <p className="spork-muted" style={{ maxWidth: 360 }}>
          Point Spork at any folder (an existing repo or an empty dir) and it
          captures your code as a root node on the graph. Spork keeps its own state
          in a hidden <code>.spork/</code> folder inside the project (git-ignored,
          never committed).
        </p>
        <div className="spork-empty-actions">
          <input
            type="text"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder={native ? "Choose a folder, or type a path…" : "/path/to/project"}
            aria-label="Project path"
            disabled={busy}
            style={{ width: 240 }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void primary();
            }}
          />
          {native && (
            <button
              className="btn"
              onClick={() => void browse()}
              disabled={busy}
              aria-label="Browse for a folder"
            >
              <Icon name="folder" size={14} /> Browse…
            </button>
          )}
          <button
            className="btn btn--primary"
            onClick={() => void primary()}
            disabled={busy}
          >
            <Icon name="download" size={14} />{" "}
            {busy ? "Importing…" : "Open project"}
          </button>
        </div>
        {error && (
          <p className="spork-error" role="alert" style={{ maxWidth: 360 }}>
            {error}
          </p>
        )}
      </div>
    </div>
  );
}
