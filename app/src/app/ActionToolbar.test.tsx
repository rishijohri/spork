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
  setDispatchError,
  fakeUlid,
} from "../ipc/mock";
import type { ActivityLevel } from "../state/store";
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

  it("disables every selection-gated action when nothing is selected", () => {
    render(<ActionToolbar node={null} ariaLabel="Actions" />);
    // With no selection, all the gated actions are disabled (there is no
    // always-on action in the Spork toolbar — every action needs a node).
    expect(screen.getByRole("button", { name: "View" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Validate" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Restore" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New Branch" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Commit to GitHub" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Push to GitHub" })).toBeDisabled();
  });

  it("New Branch requires a selection (disabled with none, enabled with one)", () => {
    const { rerender } = render(
      <ActionToolbar node={null} ariaLabel="Actions" />,
    );
    expect(screen.getByRole("button", { name: "New Branch" })).toBeDisabled();
    rerender(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(screen.getByRole("button", { name: "New Branch" })).toBeEnabled();
  });

  it("enables Validate + Restore + GitHub actions on a snapshot-owning mutating node", () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    expect(screen.getByRole("button", { name: "Validate" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Restore" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Commit to GitHub" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Push to GitHub" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "View" })).toBeEnabled();
  });

  it("disables snapshot-only actions on an observing node without a snapshot", () => {
    render(<ActionToolbar node={checkNode()} ariaLabel="Actions" />);
    expect(screen.getByRole("button", { name: "Validate" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Restore" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Commit to GitHub" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Push to GitHub" })).toBeDisabled();
    // ...but a generic selection action (View) and New Branch are still on.
    expect(screen.getByRole("button", { name: "View" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "New Branch" })).toBeEnabled();
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

  it("Restore dispatches a NODE_RESTORE for the selected node", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Restore" })).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find(
      (c) => c.command === "NODE_RESTORE",
    );
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "NODE_RESTORE") {
      expect(cmd.nodeId).toBe(EDIT_ID);
    }
  });

  it("New Branch dispatches a BRANCH_FORK off the selected node", async () => {
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "New Branch" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "New Branch" })).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find(
      (c) => c.command === "BRANCH_FORK",
    );
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "BRANCH_FORK") {
      expect(cmd.fromNodeId).toBe(EDIT_ID);
      expect(cmd.name).toBe(`branch/${EDIT_ID}`);
    }
  });

  it("Commit to GitHub dispatches GIT_EXPORT on a snapshot-owning node", async () => {
    setDispatchReply("GIT_EXPORT", {
      result: "GIT",
      branch: `spork/${EDIT_ID}`,
      commitSha: "0".repeat(40),
      pushed: false,
    });
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Commit to GitHub" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Commit to GitHub" }),
      ).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find((c) => c.command === "GIT_EXPORT");
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "GIT_EXPORT") {
      expect(cmd.nodeId).toBe(EDIT_ID);
      expect(cmd.branch).toBeNull();
    }
    // A git action is action-shaped (no opId), so it registers no optimistic op.
    expect(Object.keys(useUiStore.getState().pending)).toHaveLength(0);
  });

  it("Push to GitHub dispatches GIT_PUSH on a snapshot-owning node", async () => {
    setDispatchReply("GIT_PUSH", {
      result: "GIT",
      branch: `spork/${EDIT_ID}`,
      commitSha: "0".repeat(40),
      pushed: true,
    });
    render(<ActionToolbar node={editNode()} ariaLabel="Actions" />);
    fireEvent.click(screen.getByRole("button", { name: "Push to GitHub" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Push to GitHub" }),
      ).toBeEnabled(),
    );
    const cmd = getDispatchedCommands().find((c) => c.command === "GIT_PUSH");
    expect(cmd).toBeDefined();
    if (cmd && cmd.command === "GIT_PUSH") {
      expect(cmd.nodeId).toBe(EDIT_ID);
      expect(cmd.remote).toBeNull();
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
    const restore = TOOLBAR_ACTIONS.find((a) => a.id === "restore")!;

    const begun: string[] = [];
    const outcome = await runToolbarAction(
      restore,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      { beginOptimistic: (opId) => begun.push(opId) },
    );

    expect(outcome.command).toEqual({
      command: "NODE_RESTORE",
      nodeId: EDIT_ID,
    });
    expect(outcome.opId).toBe(OP);
    expect(begun).toEqual([OP]);
  });

  it("returns {opId:null} for an action-shaped git command (no optimistic op)", async () => {
    setDispatchReply("GIT_EXPORT", {
      result: "GIT",
      branch: `spork/${EDIT_ID}`,
      commitSha: "0".repeat(40),
      pushed: false,
    });
    const gitExport = TOOLBAR_ACTIONS.find((a) => a.id === "gitExport")!;

    const begun: string[] = [];
    const outcome = await runToolbarAction(
      gitExport,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      { beginOptimistic: (opId) => begun.push(opId) },
    );

    // The command was dispatched but a non-MUTATION reply carries no opId.
    expect(outcome.command).toEqual({
      command: "GIT_EXPORT",
      nodeId: EDIT_ID,
      branch: null,
    });
    expect(outcome.opId).toBeNull();
    expect(begun).toEqual([]);
  });

  it("is a daemon no-op for a local-only action (no command, no opId)", async () => {
    const view = TOOLBAR_ACTIONS.find((a) => a.id === "view")!;
    const outcome = await runToolbarAction(
      view,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      { beginOptimistic: () => {} },
    );
    expect(outcome).toEqual({ command: null, opId: null });
    expect(getDispatchedCommands()).toHaveLength(0);
  });

  it("appends a success activity line after a git action settles", async () => {
    setDispatchReply("GIT_EXPORT", {
      result: "GIT",
      branch: `spork/${EDIT_ID}`,
      commitSha: "abcdef0123456789".padEnd(40, "0"),
      pushed: false,
    });
    const gitExport = TOOLBAR_ACTIONS.find((a) => a.id === "gitExport")!;

    const logged: { level: ActivityLevel; text: string }[] = [];
    await runToolbarAction(
      gitExport,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      {
        beginOptimistic: () => {},
        logActivity: (level, text) => logged.push({ level, text }),
      },
    );

    expect(logged).toHaveLength(1);
    expect(logged[0]?.level).toBe("success");
    expect(logged[0]?.text).toContain("Committed");
    expect(logged[0]?.text).toContain(`spork/${EDIT_ID}`);
    // The commit SHA is truncated to its first 8 chars.
    expect(logged[0]?.text).toContain("abcdef01");
  });

  it("appends an ERROR activity line when dispatch rejects and does NOT throw", async () => {
    // Model the NetConnect-gated Push: the dispatch rejects until granted. The
    // user must SEE the failure, and runToolbarAction must not throw.
    setDispatchError("GIT_PUSH", new Error("capability denied: NetConnect"));
    const gitPush = TOOLBAR_ACTIONS.find((a) => a.id === "gitPush")!;

    const logged: { level: ActivityLevel; text: string }[] = [];
    // The promise resolves (does not reject) — runToolbarAction swallows the error.
    const outcome = await runToolbarAction(
      gitPush,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      {
        beginOptimistic: () => {},
        logActivity: (level, text) => logged.push({ level, text }),
      },
    );

    expect(outcome.opId).toBeNull();
    expect(logged).toHaveLength(1);
    expect(logged[0]?.level).toBe("error");
    expect(logged[0]?.text).toContain("capability denied: NetConnect");
  });

  it("logs an info line for a read-only action (View)", async () => {
    const view = TOOLBAR_ACTIONS.find((a) => a.id === "view")!;
    const logged: { level: ActivityLevel; text: string }[] = [];
    await runToolbarAction(
      view,
      editNode(),
      { defaultModel: "claude-sonnet-4-6" },
      {
        beginOptimistic: () => {},
        logActivity: (level, text) => logged.push({ level, text }),
      },
    );
    expect(logged).toHaveLength(1);
    expect(logged[0]?.level).toBe("info");
    expect(logged[0]?.text).toContain("Viewing");
  });
});
