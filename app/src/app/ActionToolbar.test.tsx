// Action-toolbar wiring tests (UI_UX_DESIGN.md §5.1, §5.10, §7.1).
//
// The single node-action toolbar gates its actions on the selected node
// (§14.2/§14.5) and routes most of them through a modal (so a name / remote /
// confirm is collected) — only Run-check dispatches directly via a submenu.
// These tests pin that contract: empty-state, per-kind enablement, the
// modal-open side effect, and the run-check → NODE_RUN_CHECK dispatch.
//
// Tauri is mocked (src/test/setup.ts → src/ipc/mock.ts); no daemon, no display.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
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
  };
}

/** All action buttons keyed by their data-action attribute. */
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

describe("ActionToolbar enablement (per node kind)", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("enables every action on a snapshot-owning mutating node", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    for (const action of [
      "restore",
      "runCheck",
      "newBranch",
      "merge",
      "commit",
      "push",
    ]) {
      expect(actionButton(action)).toBeEnabled();
    }
    // Visible labels match the documented contract.
    expect(actionButton("restore")).toHaveTextContent("Restore");
    expect(actionButton("runCheck")).toHaveTextContent("Run check");
    expect(actionButton("newBranch")).toHaveTextContent("Branch");
    expect(actionButton("merge")).toHaveTextContent("Merge");
    expect(actionButton("commit")).toHaveTextContent("Commit");
    expect(actionButton("push")).toHaveTextContent("Push");
  });

  it("disables snapshot-only actions on an observing node without a snapshot", () => {
    render(<ActionToolbar node={checkNode()} ariaLabel="Actions" />);
    // Snapshot-gated (isMaterializable / mutating) actions are off.
    expect(actionButton("restore")).toBeDisabled();
    expect(actionButton("runCheck")).toBeDisabled();
    expect(actionButton("commit")).toBeDisabled();
    expect(actionButton("push")).toBeDisabled();
    // Selection-only actions stay on.
    expect(actionButton("newBranch")).toBeEnabled();
    expect(actionButton("merge")).toBeEnabled();
  });
});

describe("ActionToolbar modal-opening actions", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("clicking Restore opens the restore modal for the node (no direct dispatch)", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(useUiStore.getState().modal).toBeNull();

    fireEvent.click(actionButton("restore"));

    expect(useUiStore.getState().modal).toEqual({
      kind: "restore",
      nodeId: EDIT_ID,
    });
    // The toolbar itself dispatches nothing — the modal confirms+dispatches.
    expect(getDispatchedCommands()).toHaveLength(0);
  });

  it("Branch / Merge / Commit / Push each open their own modal", () => {
    const cases: { action: string; kind: string }[] = [
      { action: "newBranch", kind: "newBranch" },
      { action: "merge", kind: "merge" },
      { action: "commit", kind: "commit" },
      { action: "push", kind: "push" },
    ];
    for (const { action, kind } of cases) {
      useUiStore.getState().reset();
      const { unmount } = render(
        <ActionToolbar node={editNode()} ariaLabel="Actions" />,
      );
      fireEvent.click(actionButton(action));
      expect(useUiStore.getState().modal).toEqual({ kind, nodeId: EDIT_ID });
      expect(getDispatchedCommands()).toHaveLength(0);
      unmount();
    }
  });
});

describe("ActionToolbar run-check submenu", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("opens a check menu and dispatches NODE_RUN_CHECK for the chosen kind", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);

    // The menu is hidden until the run-check button is clicked.
    expect(
      document.querySelector('[data-check="validation"]'),
    ).not.toBeInTheDocument();

    fireEvent.click(actionButton("runCheck"));

    const validateItem = await screen.findByRole("menuitem", {
      name: /Validate/,
    });
    expect(validateItem).toHaveAttribute("data-check", "validation");
    // All three check kinds are offered.
    const menu = validateItem.closest('[role="menu"]') as HTMLElement;
    expect(within(menu).getByText("Stress")).toBeInTheDocument();
    expect(within(menu).getByText("Sanity")).toBeInTheDocument();

    fireEvent.click(validateItem);

    await waitFor(() => {
      const cmd = getDispatchedCommands().find(
        (c) => c.command === "NODE_RUN_CHECK",
      );
      expect(cmd).toBeDefined();
    });
    const cmd = getDispatchedCommands().find(
      (c) => c.command === "NODE_RUN_CHECK",
    );
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "NODE_RUN_CHECK") {
      expect(cmd.targetNodeId).toBe(EDIT_ID);
      expect(cmd.spec).toEqual({ kind: "validation" });
    }
  });
});
