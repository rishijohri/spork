// Action-toolbar wiring tests (DESIGN.md §14.2, §14.4, §14.5).
//
// Covers the three load-bearing behaviors the F3-UI bar must have:
//   1. enable/disable per node kind (the schema-driven gating, §14.5),
//   2. a click dispatches the action's frozen Command (the single mutation
//      path, A.1), and
//   3. the dispatch registers an optimistic op by its returned opId, which the
//      op-log stream then reconciles (§14.4).
//
// Tauri is mocked (src/test/setup.ts → src/ipc/mock.ts); no daemon, no display.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ActionToolbar } from "./ActionToolbar";
import { TOOLBAR_ACTIONS, runToolbarAction } from "./toolbar";
import { useUiStore } from "../state/store";
import {
  getDispatchedCommands,
  setDispatchReply,
  fakeUlid,
} from "../ipc/mock";
import type { NodeView } from "../ipc/types";

const EDIT_ID = "00000000000000000000000001";
const CHECK_ID = "00000000000000000000000002";

function editNode(): NodeView {
  return {
    id: EDIT_ID,
    kind: "codebase-edit",
    family: "mutating",
    status: "passed",
    isStale: false,
    ownsSnapshot: true,
    snapshotHash: "b3:aa",
    branchId: "main",
    parentIds: [],
    model: "gpt-4o",
  };
}

function checkNode(): NodeView {
  return {
    id: CHECK_ID,
    kind: "validation",
    family: "observing",
    status: "running",
    isStale: false,
    ownsSnapshot: false,
    snapshotHash: null,
    branchId: "main",
    parentIds: [EDIT_ID],
    model: null,
  };
}

describe("ActionToolbar enablement (per node kind)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("enables only the always-on action when nothing is selected", () => {
    render(<ActionToolbar node={null} ariaLabel="Actions" />);
    // Create Process is always available; the selection-gated ones are disabled.
    expect(screen.getByRole("button", { name: "Create Process" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "View" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Validate" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Recalibrate" })).toBeDisabled();
  });

  it("enables Validate + Recalibrate on a snapshot-owning mutating node", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(screen.getByRole("button", { name: "Validate" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Recalibrate" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "View" })).toBeEnabled();
  });

  it("disables Validate + Recalibrate on an observing node without a snapshot", () => {
    render(<ActionToolbar node={checkNode()} ariaLabel="Actions" />);
    expect(screen.getByRole("button", { name: "Validate" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Recalibrate" })).toBeDisabled();
    // ...but a generic selection action is still on.
    expect(screen.getByRole("button", { name: "View" })).toBeEnabled();
  });
});

describe("ActionToolbar dispatch (wired actions)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("Validate dispatches a NODE_RUN_CHECK against the selected node", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Validate" }));
    // Wait for the in-flight dispatch to settle (button re-enables).
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Validate" })).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find(
      (c) => c.command === "NODE_RUN_CHECK",
    );
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "NODE_RUN_CHECK") {
      expect(cmd.targetNodeId).toBe(EDIT_ID);
    }
  });

  it("Recalibrate dispatches a NODE_RESTORE for the selected node", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Recalibrate" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Recalibrate" })).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find(
      (c) => c.command === "NODE_RESTORE",
    );
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "NODE_RESTORE") {
      expect(cmd.nodeId).toBe(EDIT_ID);
    }
  });

  it("Create Process dispatches a NODE_CREATE carrying the top-bar model", async () => {
    useUiStore.getState().setDefaultModel("o1-preview");
    render(<ActionToolbar node={null} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Create Process" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Create Process" }),
      ).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find((c) => c.command === "NODE_CREATE");
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "NODE_CREATE") {
      expect(cmd.kind).toBe("snapshot");
      expect((cmd.payload as { model?: string }).model).toBe("o1-preview");
    }
  });

  it("a read-only action (View) dispatches nothing to the daemon", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "View" }));
    // The button settles back to enabled (no in-flight dispatch) and nothing
    // reached the daemon.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "View" })).toBeEnabled(),
    );
    expect(getDispatchedCommands()).toHaveLength(0);
  });

  it("a dispatched mutation registers an optimistic op by its returned opId", async () => {
    const OP = fakeUlid();
    setDispatchReply("NODE_RUN_CHECK", { result: "MUTATION", opId: OP, ids: {} });

    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Validate" }));

    await waitFor(() => {
      expect(useUiStore.getState().pending[OP]).toBeDefined();
      expect(useUiStore.getState().pending[OP]?.label).toBe("Validate");
    });
    // Let the in-flight button state settle.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Validate" })).toBeEnabled(),
    );
  });
});

describe("runToolbarAction (unit)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("returns the built command + opId and begins an optimistic op", async () => {
    const OP = fakeUlid();
    setDispatchReply("NODE_RESTORE", { result: "MUTATION", opId: OP, ids: {} });
    const recalibrate = TOOLBAR_ACTIONS.find((a) => a.id === "recalibrate")!;

    const begun: string[] = [];
    const outcome = await runToolbarAction(
      recalibrate,
      editNode(),
      { defaultModel: "gpt-4o" },
      { beginOptimistic: (opId) => begun.push(opId) },
    );

    expect(outcome.command).toEqual({
      command: "NODE_RESTORE",
      nodeId: EDIT_ID,
    });
    expect(outcome.opId).toBe(OP);
    expect(begun).toEqual([OP]);
  });

  it("is a daemon no-op for a local-only action (no command, no opId)", async () => {
    const view = TOOLBAR_ACTIONS.find((a) => a.id === "view")!;
    const outcome = await runToolbarAction(
      view,
      editNode(),
      { defaultModel: "gpt-4o" },
      { beginOptimistic: () => {} },
    );
    expect(outcome).toEqual({ command: null, opId: null });
    expect(getDispatchedCommands()).toHaveLength(0);
  });
});
