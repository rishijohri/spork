// P7 UI tests: the gate verdict badge, the gated-merge command, and the
// lineage/context read affordances (DESIGN.md §8.3, §13.x).
//
// These assert (1) NodeDetails renders a gate node's verdict badge from the
// view-model `gate` field, (2) the Merge modal's "quality gate" toggle dispatches
// BRANCH_MERGE_GATED, and (3) the in-memory mock answers the P7 read commands
// (NODE_CONTEXT / NODE_HANDOFF / HISTORY_QUERY) with the inline READ shape.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NodeDetails } from "./NodeDetails";
import { Modals } from "./overlays/Modals";
import { useUiStore } from "../state/store";
import { dispatch } from "../ipc/client";
import { getDispatchedCommands } from "../ipc/mock";
import type { Command, GraphView, NodeView } from "../ipc/types";

const GATE = "00000000000000000000000009";
const FROM = "00000000000000000000000002";

function gateNode(): NodeView {
  return {
    id: GATE,
    kind: "gate",
    family: "observing",
    status: "passed",
    isStale: false,
    ownsSnapshot: false,
    snapshotHash: null,
    branchId: "main",
    parentIds: [FROM],
    model: null,
    cost: null,
    gate: {
      policyId: "merge-perf",
      transition: "merge",
      decision: "blocked",
      severity: "block",
      reasons: ["metric 'p99_latency_ms' regressed 2000bps"],
      lineageHash: "b3:abc",
      overridden: false,
    },
  };
}

function viewWith(node: NodeView): GraphView {
  return {
    schemaVersion: 1,
    nodes: [node],
    edges: [],
    refs: [{ name: "HEAD", kind: "Head", target: node.id }],
  };
}

function renderWith(ui: JSX.Element) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  useUiStore.setState({ view: { schemaVersion: 1, nodes: [], edges: [], refs: [] } });
});

describe("P7 gate verdict badge", () => {
  it("renders a gate node's blocked verdict in the Info tab", async () => {
    const node = gateNode();
    useUiStore.setState({ view: viewWith(node), selectedNodeId: node.id });
    renderWith(<NodeDetails />);
    // Switch to the Info tab, where the gate badge renders.
    fireEvent.click(screen.getByRole("tab", { name: "Info" }));
    await waitFor(() => expect(screen.getByText("blocked")).toBeTruthy());
    expect(screen.getByText(/p99_latency_ms.*regressed/)).toBeTruthy();
  });
});

describe("P7 gated merge", () => {
  it("the gate toggle dispatches BRANCH_MERGE_GATED", async () => {
    const from: NodeView = { ...gateNode(), id: FROM, kind: "codebase-edit", family: "mutating", gate: null, ownsSnapshot: true, parentIds: [] };
    useUiStore.setState({ view: viewWith(from) });
    useUiStore.getState().openModal({ kind: "merge", nodeId: FROM });
    renderWith(<Modals />);

    fireEvent.click(await screen.findByLabelText(/quality gate/i));
    fireEvent.click(screen.getByRole("button", { name: /merge with gate/i }));

    await waitFor(() => {
      const gated = getDispatchedCommands().find(
        (c): c is Extract<Command, { command: "BRANCH_MERGE_GATED" }> =>
          c.command === "BRANCH_MERGE_GATED",
      );
      expect(gated).toBeTruthy();
      expect(gated?.fromNodeId).toBe(FROM);
    });
  });
});

describe("P7 read commands (mock)", () => {
  it("NODE_CONTEXT returns a READ with a prefix hash + trace", async () => {
    const res = await dispatch({ command: "NODE_CONTEXT", nodeId: FROM });
    expect(res.result).toBe("READ");
    if (res.result === "READ") {
      const data = res.data as { prefixHash: string; selectionTrace: unknown[] };
      expect(typeof data.prefixHash).toBe("string");
      expect(Array.isArray(data.selectionTrace)).toBe(true);
    }
  });

  it("NODE_HANDOFF returns a regenerable handoff", async () => {
    const res = await dispatch({ command: "NODE_HANDOFF", nodeId: FROM });
    expect(res.result).toBe("READ");
    if (res.result === "READ") {
      expect((res.data as { regenerable: boolean }).regenerable).toBe(true);
    }
  });

  it("HISTORY_QUERY tools/list returns the six read-only tools", async () => {
    const res = await dispatch({
      command: "HISTORY_QUERY",
      request: { jsonrpc: "2.0", id: 1, method: "tools/list" },
    });
    expect(res.result).toBe("READ");
    if (res.result === "READ") {
      const data = res.data as { result: { tools: unknown[] } };
      expect(data.result.tools).toHaveLength(6);
    }
  });
});
