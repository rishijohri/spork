// IPC client tests (DESIGN.md §14.1, §14.4) — against the in-memory Tauri mock.

import { describe, it, expect } from "vitest";
import {
  dispatch,
  graphView,
  openProject,
  listenOpLog,
  isTauri,
} from "./client";
import {
  setMockGraphView,
  setDispatchReply,
  getDispatchedCommands,
  getOpenedProjects,
  emitOpLogEvent,
} from "./mock";
import type { GraphView } from "./types";

const NODE = "00000000000000000000000001";

function fixtureView(): GraphView {
  return {
    schemaVersion: 1,
    nodes: [
      {
        id: NODE,
        kind: "codebase-edit",
        family: "mutating",
        status: "passed",
        isStale: false,
        ownsSnapshot: true,
        snapshotHash: "b3:deadbeef",
        branchId: "main",
        parentIds: [],
        model: "claude-sonnet-4-6",
      },
    ],
    edges: [],
    refs: [{ name: "HEAD", kind: "Head", target: NODE }],
  };
}

describe("ipc client", () => {
  it("dispatch serializes a Command and returns the parsed CommandResult", async () => {
    setDispatchReply("NODE_DIFF", {
      result: "DIFF",
      changedPaths: ["src/a.rs", "src/b.rs"],
    });

    const res = await dispatch({
      command: "NODE_DIFF",
      nodeId: NODE,
      against: null,
    });

    expect(res).toEqual({
      result: "DIFF",
      changedPaths: ["src/a.rs", "src/b.rs"],
    });
    // The exact command payload reached the backend.
    expect(getDispatchedCommands()).toEqual([
      { command: "NODE_DIFF", nodeId: NODE, against: null },
    ]);
  });

  it("a mutation dispatch returns a MUTATION result with an opId", async () => {
    const res = await dispatch({ command: "NODE_RESTORE", nodeId: NODE });
    expect(res.result).toBe("MUTATION");
    if (res.result === "MUTATION") {
      expect(typeof res.opId).toBe("string");
    }
  });

  it("graphView maps the fixture snapshot to the typed shape", async () => {
    setMockGraphView(fixtureView());
    const view = await graphView();
    expect(view.nodes[0]?.kind).toBe("codebase-edit");
    expect(view.refs[0]?.name).toBe("HEAD");
  });

  it("openProject records the path", async () => {
    await openProject("/tmp/repo");
    expect(getOpenedProjects()).toEqual(["/tmp/repo"]);
  });

  it("listenOpLog delivers replayed events to the handler", async () => {
    const seen: number[] = [];
    const unlisten = await listenOpLog((e) => seen.push(e.seq));
    emitOpLogEvent({
      type: "NODE_CREATED",
      seq: 1,
      nodeId: NODE,
      schemaVersion: 1,
    });
    emitOpLogEvent({ type: "GC_PERFORMED", seq: 2 });
    expect(seen).toEqual([1, 2]);
    unlisten();
    emitOpLogEvent({ type: "GC_PERFORMED", seq: 3 });
    expect(seen).toEqual([1, 2]); // no delivery after unlisten
  });
});

describe("browser-mode fallback (no Tauri runtime)", () => {
  it("detects no Tauri runtime in a plain browser / jsdom", () => {
    // jsdom (like the Vite dev server's plain browser) injects no
    // `window.__TAURI_INTERNALS__`, so the client routes to the in-memory mock.
    expect(
      (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
    ).toBeUndefined();
    expect(isTauri()).toBe(false);
  });

  it("listenOpLog does NOT throw without a Tauri runtime (it uses the mock)", async () => {
    // The real Tauri `listen()` throws "Cannot read properties of undefined
    // (reading 'transformCallback')" with no runtime; the mock path must not.
    let unlisten: (() => void) | undefined;
    await expect(
      (async () => {
        unlisten = await listenOpLog(() => {});
      })(),
    ).resolves.toBeUndefined();
    expect(typeof unlisten).toBe("function");
    unlisten?.();
  });

  it("dispatch routes through the mock with no Tauri runtime", async () => {
    // A dispatch resolves against the in-memory mock rather than throwing.
    const res = await dispatch({ command: "GC_RUN", dryRun: true });
    expect(res.result).toBe("GC");
    expect(getDispatchedCommands().some((c) => c.command === "GC_RUN")).toBe(
      true,
    );
  });
});
