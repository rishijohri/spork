// End-to-end-ish shell flow (DESIGN.md §14.4) — the single reconciliation path.
//
// This is the test the F3-UI bar's optimistic-UI contract turns on: a dispatched
// MUTATION returns only an `opId` (+ minted nodeId); the resulting graph state
// arrives over the op-log event stream and the pure reducer folds it into the
// live view-model — which the canvas then renders. Tauri is mocked end to end
// (invoke + listen), so we drive the real Shell wiring (graph_view seed → op-log
// subscription → toolbar dispatch → replayed event → view update) with no daemon
// and no display.
//
// The toolbar now renders ONCE (top bar). Run-check is a dropdown; the durable
// state arrives only via the stream. Ephemeral frames update the STORE rail (the
// new RunRail renders the Activity tab, not RUN_STDOUT — that's forward-map).

import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent, waitFor, act } from "@testing-library/react";
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

  it("a dispatched run-check mutation updates the view via replayed op-log events", async () => {
    setMockGraphView(seedView());
    // The mutation replies with only a correlation opId + the minted nodeId —
    // never the new graph state. The minted id is the reconciliation key.
    setDispatchReply("NODE_RUN_CHECK", {
      result: "MUTATION",
      opId: OP,
      ids: { nodeId: CREATED },
    });

    renderShell();

    // The graph_view fetch seeds the live view-model: the root node renders.
    await waitFor(() => {
      expect(useUiStore.getState().view.nodes).toHaveLength(1);
    });

    // Select the root node so the single top-bar toolbar gates on it.
    act(() => {
      useUiStore.getState().selectNode(ROOT);
    });

    // The toolbar renders ONCE now (top bar) — a single runCheck button. Open the
    // Run-check dropdown, then pick Validate to dispatch NODE_RUN_CHECK.
    const runCheckBtn = await waitFor(() => {
      const btns = document.querySelectorAll<HTMLButtonElement>(
        '[data-action="runCheck"]',
      );
      expect(btns).toHaveLength(1);
      return btns[0]!;
    });
    fireEvent.click(runCheckBtn);

    const validationItem = await waitFor(() => {
      const item = document.querySelector<HTMLButtonElement>(
        '[data-check="validation"]',
      );
      expect(item).not.toBeNull();
      return item!;
    });
    fireEvent.click(validationItem);

    // The frozen NODE_RUN_CHECK command reached the daemon, targeting the root,
    // and its returned opId is registered as an in-flight optimistic op (§14.4).
    await waitFor(() => {
      const runCheck = getDispatchedCommands().find(
        (c) => c.command === "NODE_RUN_CHECK",
      );
      expect(runCheck).toBeDefined();
      expect(
        runCheck?.command === "NODE_RUN_CHECK" ? runCheck.targetNodeId : null,
      ).toBe(ROOT);
      expect(useUiStore.getState().pending[OP]).toBeDefined();
    });

    // The view has NOT yet grown — durable state only arrives over the stream.
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
        edge: "VALIDATES",
      });
    });

    // The replayed events updated the live view: the new node + edge are in the
    // view-model now, and the optimistic op reconciled via the live wiring alone
    // (the single reconciliation path) — no manual resolve.
    await waitFor(() => {
      const v = useUiStore.getState().view;
      expect(v.nodes.some((n) => n.id === CREATED)).toBe(true);
      expect(v.edges.some((e) => e.from === ROOT && e.to === CREATED)).toBe(
        true,
      );
      expect(v.nodes).toHaveLength(2);
      expect(useUiStore.getState().pending[OP]).toBeUndefined();
    });
  });

  it("ephemeral run frames buffer into the store rail without stalling the op-log", async () => {
    setMockGraphView(seedView());
    renderShell();

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
      // ...and an ordered op-log event still lands (the side-channel never stalls
      // the ordered path, DESIGN.md §5.5/§14.4).
      emitOpLogEvent({ type: "GC_PERFORMED", seq: 7 });
    });

    // The frames buffer onto the node's rail in the STORE (the new RunRail does
    // not render RUN_STDOUT — it's forward-map), and the ordered seq advanced.
    await waitFor(() => {
      expect(useUiStore.getState().rail[ROOT]).toHaveLength(3);
      expect(useUiStore.getState().lastSeq).toBe(7);
    });
  });
});
