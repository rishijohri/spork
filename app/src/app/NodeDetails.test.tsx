// Node-Details tests (DESIGN.md §14.5) — selection render, lazy diff fetch via
// the mocked dispatch, and per-kind toolbar enablement.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NodeDetails } from "./NodeDetails";
import { useUiStore } from "../state/store";
import { setDispatchReply, getDispatchedCommands } from "../ipc/mock";
import type { GraphView } from "../ipc/types";

const EDIT = "00000000000000000000000001";
const CHECK = "00000000000000000000000002";

function view(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      {
        id: EDIT,
        kind: "codebase-edit",
        family: "mutating",
        status: "passed",
        isStale: false,
        ownsSnapshot: true,
        snapshotHash: "b3:aa",
        branchId: "main",
        parentIds: [],
        model: "gpt-4o",
      },
      {
        id: CHECK,
        kind: "validation",
        family: "observing",
        status: "running",
        isStale: false,
        ownsSnapshot: false,
        snapshotHash: null,
        branchId: "main",
        parentIds: [EDIT],
        model: null,
      },
    ],
    edges: [],
    refs: [],
  };
}

function renderDetails() {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={qc}>
      <NodeDetails />
    </QueryClientProvider>,
  );
}

describe("NodeDetails", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
    useUiStore.getState().setView(view());
  });

  it("prompts to select a node when nothing is selected", () => {
    renderDetails();
    expect(
      screen.getByText(/select a node to see its details/i),
    ).toBeInTheDocument();
  });

  it("renders the selected node's descriptor + id", () => {
    useUiStore.getState().selectNode(EDIT);
    renderDetails();
    expect(screen.getByText("Edit")).toBeInTheDocument();
    expect(screen.getByText(EDIT)).toBeInTheDocument();
  });

  it("enables Validate/Restore on a snapshot-owning mutating node", () => {
    useUiStore.getState().selectNode(EDIT);
    renderDetails();
    const validate = screen.getAllByRole("button", { name: "Validate" })[0];
    const restore = screen.getAllByRole("button", {
      name: "Restore",
    })[0];
    expect(validate).toBeEnabled();
    expect(restore).toBeEnabled();
  });

  it("disables Validate/Restore on an observing node without a snapshot", () => {
    useUiStore.getState().selectNode(CHECK);
    renderDetails();
    const validate = screen.getAllByRole("button", { name: "Validate" })[0];
    const restore = screen.getAllByRole("button", {
      name: "Restore",
    })[0];
    expect(validate).toBeDisabled();
    expect(restore).toBeDisabled();
  });

  it("the diff tab requests changed paths and a blob via the mocked dispatch", async () => {
    setDispatchReply("NODE_DIFF", {
      result: "DIFF",
      changedPaths: ["src/lib.rs"],
    });
    setDispatchReply("BLOB_READ", {
      result: "BLOB",
      bytes: Array.from(new TextEncoder().encode("fn main() {}")),
    });

    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    fireEvent.click(screen.getByRole("tab", { name: "diff" }));

    // The changed-path list arrives from the NODE_DIFF dispatch.
    const pathBtn = await screen.findByRole("button", { name: "src/lib.rs" });
    expect(pathBtn).toBeInTheDocument();

    // Opening the file fires a BLOB_READ dispatch.
    fireEvent.click(pathBtn);
    await waitFor(() => {
      const tags = getDispatchedCommands().map((c) => c.command);
      expect(tags).toContain("NODE_DIFF");
      expect(tags).toContain("BLOB_READ");
    });
  });
});
