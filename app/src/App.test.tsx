// App smoke test (the trivial test that wires the runner) plus a five-region
// shell mount check against the mocked Tauri surface.

import { describe, it, expect } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import App from "./App";
import { setMockGraphView } from "./ipc/mock";

describe("App", () => {
  it("renders the five-region shell with the legend", async () => {
    setMockGraphView({ schemaVersion: 1, nodes: [], edges: [], refs: [] });
    render(<App />);
    // The legend titles the node-type list — proof the shell mounted.
    await waitFor(() => {
      expect(screen.getByText("Node types")).toBeInTheDocument();
    });
    // The top-bar model selector is present.
    expect(
      screen.getByLabelText("Default model for new nodes"),
    ).toBeInTheDocument();
  });
});
