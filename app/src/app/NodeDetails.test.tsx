// Node-Details tests (DESIGN.md §14.5) — empty state, selection header, the
// Changes/Info tabs (no Conversation/Results), Info-tab fields, and the real
// lazy diff fetch via the mocked NODE_DIFF dispatch.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NodeDetails } from "./NodeDetails";
import { useUiStore } from "../state/store";
import { setDispatchReply } from "../ipc/mock";
import { shortId } from "../ui/format";
import type { GraphView } from "../ipc/types";

const EDIT = "00000000000000000000000001";
const CHECK = "00000000000000000000000002";

function view(): GraphView {
  return {
    schemaVersion: 2,
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
        model: "openai/gpt-4o",
        cost: null,
        gate: null,
        presentationStatus: null,
        lineLabel: "main",
        forkedFrom: null,
      },
      {
        id: CHECK,
        kind: "validation",
        family: "observing",
        status: "running",
        isStale: false,
        ownsSnapshot: false,
        snapshotHash: null,
        branchId: "feature-x",
        parentIds: [EDIT],
        model: null,
        cost: null,
        gate: null,
        presentationStatus: null,
        lineLabel: "feature-x",
        forkedFrom: null,
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
      screen.getByText(/select a node to inspect it/i),
    ).toBeInTheDocument();
  });

  it("renders the selected node's descriptor label, short id, and the two tabs", () => {
    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    // Descriptor label for "codebase-edit" is "Edit".
    expect(screen.getByText("Edit")).toBeInTheDocument();
    // Short id (entropy tail) is shown, not the raw 26-char ULID.
    expect(screen.getByText(shortId(EDIT))).toBeInTheDocument();
    expect(screen.queryByText(EDIT)).toBeNull();

    // A mutating Edit node leads with Changes, then Thread (was Conversation,
    // REALIGNMENT_PLAN §5c), then Info — family-driven.
    expect(screen.getByRole("tab", { name: "Changes" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Thread" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Info" })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(3);
  });

  it("renders the node's transcript in the Thread tab (P7.5 W5b)", async () => {
    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    fireEvent.click(screen.getByRole("tab", { name: "Thread" }));

    // The mock's get_node_transcript returns a sample assistant turn; it renders.
    await waitFor(() => {
      expect(
        screen.getByText(/captures the project as the root snapshot/i),
      ).toBeInTheDocument();
    });
    // The threaded follow-up composer is present.
    expect(screen.getByLabelText(/Follow-up/i)).toBeInTheDocument();
  });

  it("shows Line, Model, and Parents in the Info tab", () => {
    useUiStore.getState().selectNode(CHECK);
    renderDetails();

    fireEvent.click(screen.getByRole("tab", { name: "Info" }));

    // The line is shown by its friendly label (was a raw "Branch" — §5a/§5c).
    expect(screen.getByText("Line")).toBeInTheDocument();
    expect(screen.getByText("feature-x")).toBeInTheDocument();

    // Parents section: this node's parent is EDIT, surfaced as a short-id link.
    expect(screen.getByText("Parents")).toBeInTheDocument();
    const parentLink = screen.getByRole("button", { name: shortId(EDIT) });
    expect(parentLink).toBeInTheDocument();

    // Clicking the parent link re-selects the parent node.
    fireEvent.click(parentLink);
    expect(useUiStore.getState().selectedNodeId).toBe(EDIT);
  });

  it("shows the humanized model on a node that carries one", () => {
    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    fireEvent.click(screen.getByRole("tab", { name: "Info" }));

    expect(screen.getByText("Model")).toBeInTheDocument();
    // humanizeModel drops the provider prefix.
    expect(screen.getByText("gpt-4o")).toBeInTheDocument();
  });

  it("renders the per-node cost (P6) when the node carries one", () => {
    // Seed a costed node (an agent-run attaches one) and assert the Cost row
    // renders the formatted dollar amount + token up/down counts.
    const v = view();
    v.nodes[0]!.cost = { inputTokens: 1200, outputTokens: 300, microUsd: 8_100 };
    useUiStore.getState().setView(v);
    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    fireEvent.click(screen.getByRole("tab", { name: "Info" }));
    expect(screen.getByText("Cost")).toBeInTheDocument();
    // 8100 micro-USD = $0.00810 (sub-cent precision).
    expect(screen.getByText("$0.00810")).toBeInTheDocument();
    expect(screen.getByText("(1200↑/300↓)")).toBeInTheDocument();
  });

  it("Changes tab renders the changed-path buttons from NODE_DIFF", async () => {
    setDispatchReply("NODE_DIFF", {
      result: "DIFF",
      changedPaths: ["a.ts", "b.ts"],
    });

    useUiStore.getState().selectNode(EDIT);
    renderDetails();

    // Changes is the default tab; the diff arrives via the mocked dispatch.
    const list = await screen.findByLabelText("Changed paths");
    expect(within(list).getByText("a.ts")).toBeInTheDocument();
    expect(within(list).getByText("b.ts")).toBeInTheDocument();

    // Both are clickable file buttons carrying their data-path.
    await waitFor(() => {
      expect(document.querySelector('[data-path="a.ts"]')).not.toBeNull();
      expect(document.querySelector('[data-path="b.ts"]')).not.toBeNull();
    });
  });
});
