// Canvas tests (DESIGN.md §14.3, §14.5) — pure view-model mappers + live render.
// Tauri is globally mocked; ResizeObserver is polyfilled (src/test/setup.ts).

import { describe, it, expect, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Canvas, toRfNodes, toRfEdges } from "./Canvas";
import { useUiStore } from "../state/store";
import { descriptorFor } from "./descriptors";
import {
  getDispatchedCommands,
  getOpenedProjects,
} from "../ipc/mock";
import type { GraphView, NodeView } from "../ipc/types";

const A = "00000000000000000000000001";
const B = "00000000000000000000000002";

function node(over: Partial<NodeView> & Pick<NodeView, "id" | "kind">): NodeView {
  return {
    family: "mutating",
    status: "passed",
    isStale: false,
    ownsSnapshot: false,
    snapshotHash: null,
    branchId: "main",
    parentIds: [],
    model: null,
    cost: null,
    gate: null,
    presentationStatus: null,
    lineLabel: "main",
    forkedFrom: null,
    ...over,
  };
}

function twoNodeView(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      node({
        id: A,
        kind: "codebase-edit",
        family: "mutating",
        ownsSnapshot: true,
        snapshotHash: "b3:aa",
      }),
      node({
        id: B,
        kind: "validation",
        family: "observing",
        status: "running",
        parentIds: [A],
      }),
    ],
    edges: [{ from: A, to: B, edgeType: "VALIDATES" }],
    refs: [],
  };
}

const emptyView: GraphView = { schemaVersion: 1, nodes: [], edges: [], refs: [] };

function renderCanvas() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <ReactFlowProvider>
        <Canvas />
      </ReactFlowProvider>
    </QueryClientProvider>,
  );
}

describe("toRfNodes", () => {
  it("maps each node to a sporkCard with node + descriptor in data, plus lane bands", () => {
    const v = twoNodeView();
    const { rfNodes, lanes } = toRfNodes(v, null, [], "", null);
    expect(rfNodes).toHaveLength(2);
    // Both demo nodes are on `main`, so there is one swimlane.
    expect(lanes).toHaveLength(1);
    expect(lanes[0]?.branchId).toBe("main");

    const a = rfNodes.find((n) => n.id === A);
    expect(a?.type).toBe("sporkCard");
    expect(a?.data.node.id).toBe(A);
    expect(a?.data.descriptor).toEqual(descriptorFor("codebase-edit"));
  });

  it("marks only the selected node as selected", () => {
    const { rfNodes } = toRfNodes(twoNodeView(), A, [], "", null);
    expect(rfNodes.find((n) => n.id === A)?.data.selected).toBe(true);
    expect(rfNodes.find((n) => n.id === B)?.data.selected).toBe(false);
  });

  it("dims nodes whose kind is excluded by a non-empty kind filter", () => {
    const { rfNodes } = toRfNodes(twoNodeView(), null, ["codebase-edit"], "", null);
    // A's kind is in the filter (highlighted), B's is not (dimmed).
    expect(rfNodes.find((n) => n.id === A)?.data.dimmed).toBe(false);
    expect(rfNodes.find((n) => n.id === B)?.data.dimmed).toBe(true);
  });

  it("does not dim anything when the kind filter is empty", () => {
    const { rfNodes } = toRfNodes(twoNodeView(), null, [], "", null);
    expect(rfNodes.every((n) => n.data.dimmed === false)).toBe(true);
  });

  it("dims nodes that do not match the free-text search", () => {
    // "validation" matches B's kind/label but not A.
    const { rfNodes } = toRfNodes(twoNodeView(), null, [], "validation", null);
    expect(rfNodes.find((n) => n.id === A)?.data.dimmed).toBe(true);
    expect(rfNodes.find((n) => n.id === B)?.data.dimmed).toBe(false);
  });

  it("dims nodes not on the focused lane (REALIGNMENT_PLAN §5a)", () => {
    // Focusing a non-existent lane dims everything; focusing `main` dims nothing.
    const dimmed = toRfNodes(twoNodeView(), null, [], "", "other");
    expect(dimmed.rfNodes.every((n) => n.data.dimmed === true)).toBe(true);
    const onMain = toRfNodes(twoNodeView(), null, [], "", "main");
    expect(onMain.rfNodes.every((n) => n.data.dimmed === false)).toBe(true);
  });
});

describe("toRfEdges", () => {
  it("gives a VALIDATES edge a humanized label and typed className", () => {
    const rf = toRfEdges(twoNodeView());
    expect(rf).toHaveLength(1);
    expect(rf[0]?.source).toBe(A);
    expect(rf[0]?.target).toBe(B);
    expect(rf[0]?.label).toBe("validates");
    expect(rf[0]?.className).toContain("spork-edge-VALIDATES");
  });

  it("leaves a PARENT_CHILD edge label undefined", () => {
    const v: GraphView = {
      schemaVersion: 1,
      nodes: twoNodeView().nodes,
      edges: [{ from: A, to: B, edgeType: "PARENT_CHILD" }],
      refs: [],
    };
    const rf = toRfEdges(v);
    expect(rf[0]?.label).toBeUndefined();
    expect(rf[0]?.className).toContain("spork-edge-PARENT_CHILD");
  });
});

describe("Canvas (live render)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("renders one node card per view-model node with descriptor labels", () => {
    act(() => {
      useUiStore.getState().setView(twoNodeView());
    });
    renderCanvas();

    const cards = screen.getAllByTestId("node-card");
    expect(cards).toHaveLength(2);
    expect(screen.getByText("Edit")).toBeInTheDocument();
    expect(screen.getByText("Validation")).toBeInTheDocument();
  });

  it("shows the empty state when the view has no nodes", () => {
    act(() => {
      useUiStore.getState().setView(emptyView);
    });
    renderCanvas();

    expect(screen.getByTestId("canvas-empty")).toBeInTheDocument();
    expect(screen.queryByTestId("node-card")).toBeNull();
  });

  it("shows a loading spinner (not the picker) while a project is opening (P7.5)", () => {
    act(() => {
      useUiStore.getState().setView(emptyView);
      useUiStore.getState().setProjectOpening(true);
    });
    renderCanvas();

    // A reopened/just-opened project must NOT flash the onboarding picker.
    expect(screen.getByTestId("canvas-opening")).toBeInTheDocument();
    expect(screen.queryByTestId("canvas-empty")).toBeNull();
  });

  it("opens a project and imports it into a root node (P7.5 W1)", async () => {
    act(() => {
      useUiStore.getState().setView(emptyView);
    });
    renderCanvas();

    fireEvent.change(screen.getByLabelText("Project path"), {
      target: { value: "/my/repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: /Open project/i }));

    // The empty state drives the real open_project → PROJECT_IMPORT flow: the
    // daemon captures the working tree into the first node (the unblocker).
    await waitFor(() => {
      expect(getOpenedProjects()).toContain("/my/repo");
    });
    const imports = getDispatchedCommands().filter(
      (c) => c.command === "PROJECT_IMPORT",
    );
    expect(imports).toHaveLength(1);
    expect(imports[0]).toMatchObject({
      command: "PROJECT_IMPORT",
      branchId: "main",
      origin: "import",
    });
  });
});
