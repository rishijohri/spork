// Canvas tests (DESIGN.md §14.3, §14.5) — fixture render + schema-driven styling
// + selection-triggers-diff. Tauri is mocked; ResizeObserver is polyfilled.

import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import { Canvas, toRfNodes, toRfEdges } from "./Canvas";
import { useUiStore } from "../state/store";
import { descriptorFor } from "./descriptors";
import type { GraphView } from "../ipc/types";

const A = "00000000000000000000000001";
const B = "00000000000000000000000002";

function twoNodeView(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      {
        id: A,
        kind: "codebase-edit",
        family: "mutating",
        status: "passed",
        isStale: false,
        ownsSnapshot: true,
        snapshotHash: "b3:aa",
        branchId: "main",
        parentIds: [],
        model: null,
      },
      {
        id: B,
        kind: "validation",
        family: "observing",
        status: "running",
        isStale: false,
        ownsSnapshot: false,
        snapshotHash: null,
        branchId: "main",
        parentIds: [A],
        model: null,
      },
    ],
    edges: [{ from: A, to: B, edgeType: "VALIDATES" }],
    refs: [],
  };
}

function renderCanvas(onSelect?: (id: string) => void) {
  return render(
    <ReactFlowProvider>
      <Canvas {...(onSelect ? { onSelect } : {})} />
    </ReactFlowProvider>,
  );
}

describe("Canvas", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
    useUiStore.getState().setView(twoNodeView());
  });

  it("renders one card per view-model node with descriptor labels", () => {
    renderCanvas();
    const edit = descriptorFor("codebase-edit");
    const val = descriptorFor("validation");
    expect(
      screen.getByText(`${edit.icon} ${edit.label}`),
    ).toBeInTheDocument();
    expect(screen.getByText(`${val.icon} ${val.label}`)).toBeInTheDocument();
  });

  it("maps view-model nodes and edges into React Flow shapes from a fixture", () => {
    // React Flow only paints edge SVG paths once nodes are MEASURED, which a
    // jsdom ResizeObserver no-op cannot do; assert the deterministic mapping
    // (the part the renderer owns) directly from the fixture view.
    const v = twoNodeView();
    const rfNodes = toRfNodes(v, null);
    const rfEdges = toRfEdges(v);
    expect(rfNodes).toHaveLength(2);
    expect(rfEdges).toHaveLength(1);
    expect(rfEdges[0]?.source).toBe(A);
    expect(rfEdges[0]?.target).toBe(B);
    expect(rfEdges[0]?.label).toBe("VALIDATES");
    expect(rfEdges[0]?.className).toContain("spork-edge-VALIDATES");
  });

  it("applies the descriptor color and kind class to the node card", () => {
    const card = toRfNodes(twoNodeView(), null).find(
      (n) => n.id === A,
    );
    expect(card?.className).toContain("spork-node-codebase-edit");
    expect((card?.style as { borderColor?: string }).borderColor).toBe(
      descriptorFor("codebase-edit").color,
    );
  });

  it("renders the node card in the live canvas DOM", () => {
    const { container } = renderCanvas();
    expect(
      container.querySelector(".spork-node-codebase-edit"),
    ).not.toBeNull();
  });

  it("clicking a node sets the selection and fires onSelect (diff trigger)", async () => {
    const onSelect = vi.fn();
    renderCanvas(onSelect);

    const edit = descriptorFor("codebase-edit");
    const card = screen.getByText(`${edit.icon} ${edit.label}`);
    fireEvent.click(card);

    await waitFor(() => {
      expect(useUiStore.getState().selectedNodeId).toBe(A);
    });
    expect(onSelect).toHaveBeenCalledWith(A);
  });
});
