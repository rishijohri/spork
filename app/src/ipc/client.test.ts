// IPC client tests (DESIGN.md §14.1, §14.4) — against the in-memory Tauri mock.

import { describe, it, expect } from "vitest";
import { dispatch, graphView, openProject, listenOpLog } from "./client";
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
        model: "claude-3-5-sonnet",
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
