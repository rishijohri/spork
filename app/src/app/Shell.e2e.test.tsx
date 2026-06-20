// End-to-end-ish shell flow (DESIGN.md §14.4) — the single reconciliation path.
//
// This is the test the F3-UI bar's optimistic-UI contract turns on: a dispatched
// MUTATION returns only an `opId`; the resulting graph state arrives over the
// op-log event stream and the pure reducer folds it into the live view-model —
// which the canvas then renders. Tauri is mocked end to end (invoke + listen),
// so we drive the real Shell wiring (graph_view seed → op-log subscription →
// toolbar dispatch → replayed event → view update) with no daemon and no display.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Shell } from "./Shell";
import { useUiStore } from "../state/store";
import {
  setMockGraphView,
  setDispatchReply,
  getDispatchedCommands,
  emitOpLogEvent,
  emitEphemeral,
} from "../ipc/mock";
import { descriptorFor } from "../canvas/descriptors";
import type { GraphView } from "../ipc/types";

const ROOT = "00000000000000000000000001";
const CREATED = "00000000000000000000000099";
const OP = "0000000000000000000000000P";

function seedView(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      {
        id: ROOT,
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
    ],
    edges: [],
    refs: [{ name: "HEAD", kind: "Head", target: ROOT }],
  };
}

function renderShell() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <ReactFlowProvider>
        <Shell />
      </ReactFlowProvider>
    </QueryClientProvider>,
  );
}

describe("Shell end-to-end-ish flow (mocked Tauri)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("a dispatched mutation updates the view via a replayed op-log event", async () => {
    setMockGraphView(seedView());
    // The mutation replies with only a correlation opId + the minted nodeId —
    // never the new graph state. The minted id is the reconciliation key. We use
    // Validate (NODE_RUN_CHECK), which mints the new observing-result node id.
    setDispatchReply("NODE_RUN_CHECK", {
      result: "MUTATION",
      opId: OP,
      ids: { nodeId: CREATED },
    });

    renderShell();

    // The graph_view fetch seeds the live view-model: the root node renders.
    const editDesc = descriptorFor("codebase-edit");
    await waitFor(() => {
      expect(
        screen.getByText(`${editDesc.icon} ${editDesc.label}`),
      ).toBeInTheDocument();
    });
    expect(useUiStore.getState().view.nodes).toHaveLength(1);

    // Select the root node, then dispatch a mutation from the wired top-bar
    // toolbar (Validate runs an observing check against the selection).
    act(() => {
      useUiStore.getState().selectNode(ROOT);
    });
    // Both the top bar and the node-details panel render the toolbar, so there
    // are two "Validate" buttons once a node is selected; drive the first.
    const validateBtn = screen.getAllByRole("button", { name: "Validate" })[0]!;
    fireEvent.click(validateBtn);

    // The frozen NODE_RUN_CHECK command reached the daemon, and its returned opId
    // is registered as an in-flight optimistic op (DESIGN.md §14.4).
    await waitFor(() => {
      expect(
        getDispatchedCommands().some((c) => c.command === "NODE_RUN_CHECK"),
      ).toBe(true);
      expect(useUiStore.getState().pending[OP]).toBeDefined();
    });
    // The button's in-flight state settles back.
    await waitFor(() =>
      expect(
        screen.getAllByRole("button", { name: "Validate" })[0],
      ).toBeEnabled(),
    );

    // The view has NOT yet grown — state only arrives over the op-log stream.
    expect(useUiStore.getState().view.nodes).toHaveLength(1);

    // The backend now forwards the resulting durable events. Replaying them
    // through the (mocked) "oplog-event" channel folds them into the view-model.
    act(() => {
      emitOpLogEvent({
        type: "NODE_CREATED",
        seq: 1,
        nodeId: CREATED,
        schemaVersion: 1,
      });
      emitOpLogEvent({
        type: "EDGE_ADDED",
        seq: 2,
        from: ROOT,
        to: CREATED,
        edge: "PARENT_CHILD",
      });
    });

    // The replayed events updated the live view: the new node + edge are now in
    // the view-model, and a second card renders on the canvas.
    await waitFor(() => {
      const v = useUiStore.getState().view;
      expect(v.nodes.some((n) => n.id === CREATED)).toBe(true);
      expect(
        v.edges.some((e) => e.from === ROOT && e.to === CREATED),
      ).toBe(true);
      expect(v.nodes).toHaveLength(2);
    });

    // The NODE_CREATED event for the minted id reconciled the optimistic op via
    // the live wiring alone (the single reconciliation path) — no manual resolve.
    expect(useUiStore.getState().pending[OP]).toBeUndefined();
  });

  it("ephemeral run frames stream into the run rail without blocking the op-log", async () => {
    setMockGraphView(seedView());
    renderShell();

    // Select the root node so the bottom rail targets it.
    await waitFor(() => {
      expect(useUiStore.getState().view.nodes).toHaveLength(1);
    });
    act(() => {
      useUiStore.getState().selectNode(ROOT);
      // A flood of ephemeral frames arrives off the side-channel...
      for (let i = 0; i < 3; i += 1) {
        emitEphemeral({
          nodeId: ROOT,
          channel: "RUN_STDOUT",
          data: `line ${i}\n`,
        });
      }
      // ...and an ordered op-log event still lands (the side-channel never
      // stalls the ordered path, DESIGN.md §5.5/§14.4).
      emitOpLogEvent({ type: "GC_PERFORMED", seq: 7 });
    });

    await waitFor(() => {
      const log = screen.getByTestId("runrail-log");
      expect(log.textContent).toBe("line 0\nline 1\nline 2\n");
      expect(useUiStore.getState().lastSeq).toBe(7);
    });
  });
});
