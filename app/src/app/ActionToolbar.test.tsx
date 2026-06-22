// Action-toolbar wiring tests (UI_UX_DESIGN.md §5.1, §5.10, §7.1;
// REALIGNMENT_PLAN §5a).
//
// The reframed toolbar: a "+ New node from here ▾" menu (Agentic / Action groups)
// plus the direct node-ops Restore + Merge-into-line. Branching is automatic, so
// there is no "new branch" verb. These tests pin that contract: empty-state, the
// node-op gating + modal-open, the new-node menu's items + their dispatch.
//
// Tauri is mocked (src/test/setup.ts → src/ipc/mock.ts); no daemon, no display.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ActionToolbar } from "./ActionToolbar";
import { useUiStore } from "../state/store";
import { getDispatchedCommands } from "../ipc/mock";
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
    cost: null,
    gate: null,
    presentationStatus: null,
    lineLabel: "main",
    forkedFrom: null,
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
    cost: null,
    gate: null,
    presentationStatus: null,
    lineLabel: "main",
    forkedFrom: null,
  };
}

function actionButtons(): HTMLButtonElement[] {
  return Array.from(
    document.querySelectorAll<HTMLButtonElement>("button[data-action]"),
  );
}

function actionButton(action: string): HTMLButtonElement {
  const btn = document.querySelector<HTMLButtonElement>(
    `button[data-action="${action}"]`,
  );
  if (!btn) throw new Error(`no button for data-action="${action}"`);
  return btn;
}

function newNodeItem(id: string): HTMLButtonElement {
  const btn = document.querySelector<HTMLButtonElement>(
    `button[data-newnode="${id}"]`,
  );
  if (!btn) throw new Error(`no new-node item "${id}"`);
  return btn;
}

describe("ActionToolbar empty state", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("renders the empty prompt and no action buttons with no selection", () => {
    render(<ActionToolbar node={null} ariaLabel="Actions" />);
    expect(screen.getByText("Select a node to act on it")).toBeInTheDocument();
    expect(actionButtons()).toHaveLength(0);
  });
});

describe("ActionToolbar node-ops (Restore / Merge)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("shows the New-node trigger plus Restore + Merge-into-line on a mutating node", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(actionButton("newNode")).toBeEnabled();
    expect(actionButton("restore")).toBeEnabled();
    expect(actionButton("merge")).toBeEnabled();
    expect(actionButton("restore")).toHaveTextContent("Restore");
    expect(actionButton("merge")).toHaveTextContent("Merge into line");
    // There is no "new branch" verb — branching is automatic.
    expect(document.querySelector('[data-action="newBranch"]')).toBeNull();
  });

  it("disables Restore on an observing node without a snapshot; Merge stays on", () => {
    render(<ActionToolbar node={checkNode()} ariaLabel="Actions" />);
    expect(actionButton("restore")).toBeDisabled();
    expect(actionButton("merge")).toBeEnabled();
  });

  it("clicking Restore opens the restore modal (no direct dispatch)", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("restore"));
    expect(useUiStore.getState().modal).toEqual({ kind: "restore", nodeId: EDIT_ID });
    expect(getDispatchedCommands()).toHaveLength(0);
  });

  it("clicking Merge opens the merge modal", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("merge"));
    expect(useUiStore.getState().modal).toEqual({ kind: "merge", nodeId: EDIT_ID });
  });
});

describe("ActionToolbar '+ New node from here' menu", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("opens to the Agentic + Action groups", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(document.querySelector('[data-newnode="ask"]')).toBeNull();
    fireEvent.click(actionButton("newNode"));
    expect(screen.getByText("Agentic")).toBeInTheDocument();
    expect(screen.getByText("Action")).toBeInTheDocument();
    // Agentic + action items are present.
    for (const id of ["ask", "plan", "explore", "work", "run-tests", "stress", "sanity", "commit", "push"]) {
      expect(newNodeItem(id)).toBeInTheDocument();
    }
  });

  it("an agentic item opens the Ask modal pre-set to its intent", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("newNode"));
    fireEvent.click(newNodeItem("plan"));
    expect(useUiStore.getState().modal).toEqual({
      kind: "askAgent",
      nodeId: EDIT_ID,
      intent: "plan",
    });
  });

  it("a commit/push item opens its modal", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("newNode"));
    fireEvent.click(newNodeItem("commit"));
    expect(useUiStore.getState().modal).toEqual({ kind: "commit", nodeId: EDIT_ID });
  });

  it("an action check item dispatches NODE_RUN_CHECK directly", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("newNode"));
    fireEvent.click(newNodeItem("run-tests"));
    await waitFor(() => {
      const cmd = getDispatchedCommands().find((c) => c.command === "NODE_RUN_CHECK");
      expect(cmd).toBeDefined();
    });
    const cmd = getDispatchedCommands().find((c) => c.command === "NODE_RUN_CHECK");
    if (cmd && cmd.command === "NODE_RUN_CHECK") {
      expect(cmd.targetNodeId).toBe(EDIT_ID);
      expect(cmd.spec).toEqual({ kind: "validation" });
    }
  });

  it("gates snapshot-needing items: Work + checks disabled on a snapshotless node", () => {
    render(<ActionToolbar node={checkNode()} ariaLabel="Actions" />);
    fireEvent.click(actionButton("newNode"));
    // Agentic read-only items stay on; Work needs a snapshot.
    expect(newNodeItem("ask")).toBeEnabled();
    expect(newNodeItem("work")).toBeDisabled();
    // Action checks + commit/push need a snapshot.
    expect(newNodeItem("run-tests")).toBeDisabled();
    expect(newNodeItem("commit")).toBeDisabled();
  });
});
