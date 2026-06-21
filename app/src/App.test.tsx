// App smoke tests: the root brings its own providers (TanStack Query + React
// Flow), mounts the five-region shell, and renders the seeded graph as node
// cards once the `graph_view` read resolves.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import App from "./App";
import { setMockGraphView } from "./ipc/mock";
import { useUiStore } from "./state/store";
import type { GraphView } from "./ipc/types";

function oneNodeView(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      {
        id: "01ROOT0000000000000000000A",
        kind: "codebase-edit",
        family: "mutating",
        status: "passed",
        isStale: false,
        ownsSnapshot: true,
        snapshotHash: "b3:01ROOT0000000000000000000A",
        branchId: "main",
        parentIds: [],
        model: "claude-sonnet-4-6",
        cost: null,
      },
    ],
    edges: [],
    refs: [{ name: "HEAD", kind: "Head", target: "01ROOT0000000000000000000A" }],
  };
}

describe("App", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("mounts the shell with the spork brand", () => {
    setMockGraphView({ schemaVersion: 1, nodes: [], edges: [], refs: [] });
    expect(() => render(<App />)).not.toThrow();
    // The brand mark renders the "spork" wordmark in the top bar.
    expect(screen.getAllByText("spork").length).toBeGreaterThan(0);
  });

  it("renders a node card once the seeded graph_view loads", async () => {
    setMockGraphView(oneNodeView());
    render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("node-card")).toBeInTheDocument();
    });
    // The single seeded node is a codebase-edit, labeled "Edit" on its card.
    const card = screen.getByTestId("node-card");
    expect(card).toHaveTextContent("Edit");
  });
});
