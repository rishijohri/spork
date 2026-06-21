// P6 agent-run UI tests: the Ask-agent modal dispatches NODE_AGENT_RUN and
// attaches a priced context node (DESIGN.md §6.6, §12.x).
//
// The modal resolves the model selector, the daemon (here the in-memory mock)
// prices the turn, and the action upserts the attached context node from the
// authoritative reply — so the model + cost show immediately. These tests assert
// the dispatched command shape, the attached node, and the per-node cost.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Modals } from "./overlays/Modals";
import { useUiStore } from "../state/store";
import { getDispatchedCommands } from "../ipc/mock";
import type { Command, GraphView } from "../ipc/types";

const TARGET = "00000000000000000000000001";

function view(): GraphView {
  return {
    schemaVersion: 2,
    nodes: [
      {
        id: TARGET,
        kind: "codebase-edit",
        family: "mutating",
        status: "passed",
        isStale: false,
        ownsSnapshot: true,
        snapshotHash: "b3:aa",
        branchId: "main",
        parentIds: [],
        model: "anthropic/claude-sonnet-4-6",
        cost: null,
        gate: null,
        presentationStatus: null,
        lineLabel: "main",
        forkedFrom: null,
      },
    ],
    edges: [],
    refs: [{ name: "HEAD", kind: "Head", target: TARGET }],
  };
}

function renderModals() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <Modals />
    </QueryClientProvider>,
  );
}

/** The last NODE_AGENT_RUN the mock recorded. */
function lastAgentRun(): Extract<Command, { command: "NODE_AGENT_RUN" }> | undefined {
  const runs = getDispatchedCommands().filter(
    (c): c is Extract<Command, { command: "NODE_AGENT_RUN" }> =>
      c.command === "NODE_AGENT_RUN",
  );
  return runs[runs.length - 1];
}

describe("Ask-agent modal (NODE_AGENT_RUN)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
    useUiStore.getState().setView(view());
    useUiStore.getState().openModal({ kind: "askAgent", nodeId: TARGET });
  });

  it("dispatches NODE_AGENT_RUN with the prompt, model, privacy, and intent", async () => {
    renderModals();
    fireEvent.change(screen.getByLabelText("Prompt"), {
      target: { value: "what does this do?" },
    });
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "local/llama3.1" },
    });
    fireEvent.change(screen.getByLabelText("Intent"), {
      target: { value: "analysis" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));

    await waitFor(() => expect(lastAgentRun()).toBeDefined());
    const cmd = lastAgentRun()!;
    expect(cmd.targetNodeId).toBe(TARGET);
    expect(cmd.prompt).toBe("what does this do?");
    expect(cmd.modelKey).toBe("local/llama3.1");
    expect(cmd.privacy).toBe("any");
    expect(cmd.intent).toBe("analysis");
  });

  it("the 'change' intent dispatches NODE_AGENT_EDIT and attaches an Edit node (P7.5 W4)", async () => {
    renderModals();
    fireEvent.change(screen.getByLabelText("Prompt"), {
      target: { value: "add a retry" },
    });
    fireEvent.change(screen.getByLabelText("Intent"), {
      target: { value: "change" },
    });
    // The action button relabels to "Make change".
    fireEvent.click(screen.getByRole("button", { name: "Make change" }));

    await waitFor(() => {
      const edits = getDispatchedCommands().filter(
        (c) => c.command === "NODE_AGENT_EDIT",
      );
      expect(edits).toHaveLength(1);
    });
    const edit = getDispatchedCommands().find(
      (c): c is Extract<Command, { command: "NODE_AGENT_EDIT" }> =>
        c.command === "NODE_AGENT_EDIT",
    )!;
    expect(edit.targetNodeId).toBe(TARGET);
    expect(edit.prompt).toBe("add a retry");
    // No read-only NODE_AGENT_RUN was sent for a change.
    expect(lastAgentRun()).toBeUndefined();

    // The Edit node was upserted into the view (renders as a codebase-edit child).
    await waitFor(() => {
      const editNodes = useUiStore
        .getState()
        .view.nodes.filter((n) => n.kind === "codebase-edit" && n.id !== TARGET);
      expect(editNodes.length).toBe(1);
    });
  });

  it("local-only checkbox sends privacy=local_only", async () => {
    renderModals();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "q" } });
    fireEvent.click(screen.getByRole("checkbox")); // the local-only privacy toggle
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));
    await waitFor(() => expect(lastAgentRun()).toBeDefined());
    expect(lastAgentRun()!.privacy).toBe("local_only");
  });

  it("attaches a priced context node and shows the answer summary", async () => {
    renderModals();
    // Default model is the cloud Claude selector → a non-zero priced cost.
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "explain" } });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));

    // The attached context node appears in the view with a cost and a dotted edge.
    await waitFor(() => {
      const ctx = useUiStore
        .getState()
        .view.nodes.find((n) => n.kind === "agent-context");
      expect(ctx).toBeDefined();
      expect(ctx!.family).toBe("context");
      expect(ctx!.cost).not.toBeNull();
      expect(ctx!.cost!.microUsd).toBeGreaterThan(0);
    });
    const edge = useUiStore
      .getState()
      .view.edges.find((e) => e.to === TARGET && e.edgeType === "DERIVED_FROM");
    expect(edge).toBeDefined();

    // The result summary renders inside the modal.
    expect(await screen.findByText(/Answered by/i)).toBeInTheDocument();
  });

  it("free local model records a zero-cost ('free') node", async () => {
    renderModals();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "q" } });
    fireEvent.change(screen.getByLabelText("Model"), {
      target: { value: "local/llama3.1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));
    await waitFor(() => {
      const ctx = useUiStore
        .getState()
        .view.nodes.find((n) => n.kind === "agent-context");
      expect(ctx?.cost?.microUsd).toBe(0);
    });
    expect(await screen.findByText(/free/i)).toBeInTheDocument();
  });
});
