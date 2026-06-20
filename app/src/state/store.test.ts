// Store tests (DESIGN.md §14.4) — optimistic correlation + ephemeral buffering.

import { describe, it, expect, beforeEach } from "vitest";
import { useUiStore, ACTIVITY_LOG_LIMIT } from "./store";

const OP = "00000000000000000000000010";
const NODE = "00000000000000000000000001";

describe("useUiStore", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("registers and resolves an optimistic mutation by opId", () => {
    const s = useUiStore.getState();
    s.beginOptimistic(OP, "NODE_CREATE");
    expect(useUiStore.getState().pending[OP]?.label).toBe("NODE_CREATE");
    s.resolveOptimistic(OP);
    expect(useUiStore.getState().pending[OP]).toBeUndefined();
  });

  it("ingests op-log events into the view-model and tracks seq", () => {
    const s = useUiStore.getState();
    s.ingestEvent({
      type: "NODE_CREATED",
      seq: 5,
      nodeId: NODE,
      schemaVersion: 1,
    });
    const state = useUiStore.getState();
    expect(state.view.nodes.find((n) => n.id === NODE)).toBeDefined();
    expect(state.lastSeq).toBe(5);
  });

  it("reconciles an optimistic op when its resulting event arrives", () => {
    // Mutation fires optimistically, recording the minted subject id (NODE). The
    // daemon's op-log event tails in referencing that id, and `ingestEvent` ALONE
    // clears the optimistic op — the single reconciliation path (the ordered
    // event stream), with no manual `resolveOptimistic`.
    const s = useUiStore.getState();
    s.beginOptimistic(OP, "NODE_CREATE", NODE);
    s.ingestEvent({
      type: "NODE_CREATED",
      seq: 1,
      nodeId: NODE,
      schemaVersion: 1,
    });
    const state = useUiStore.getState();
    expect(state.pending[OP]).toBeUndefined();
    expect(state.view.nodes).toHaveLength(1);
  });

  it("does not reconcile an op whose subject id is unrelated to the event", () => {
    // An event for a different node must leave the pending op in place.
    const OTHER = "00000000000000000000000077";
    const s = useUiStore.getState();
    s.beginOptimistic(OP, "NODE_CREATE", NODE);
    s.ingestEvent({
      type: "NODE_CREATED",
      seq: 1,
      nodeId: OTHER,
      schemaVersion: 1,
    });
    expect(useUiStore.getState().pending[OP]).toBeDefined();
  });

  it("leaves a subjectless optimistic op for explicit resolution", () => {
    // A mutation that minted no addressable subject (subjectId null) is not
    // auto-reconciled by a node event; it is cleared explicitly.
    const s = useUiStore.getState();
    s.beginOptimistic(OP, "GC_RUN", null);
    s.ingestEvent({ type: "GC_PERFORMED", seq: 1 });
    expect(useUiStore.getState().pending[OP]).toBeDefined();
    s.resolveOptimistic(OP);
    expect(useUiStore.getState().pending[OP]).toBeUndefined();
  });

  it("buffers ephemeral frames per node without blocking", () => {
    const s = useUiStore.getState();
    s.ingestEphemeral({ nodeId: NODE, channel: "RUN_STDOUT", data: "line 1\n" });
    s.ingestEphemeral({ nodeId: NODE, channel: "RUN_STDOUT", data: "line 2\n" });
    expect(useUiStore.getState().rail[NODE]).toEqual(["line 1\n", "line 2\n"]);
  });

  it("appends activity-log entries newest-last with level + text + timestamp", () => {
    const s = useUiStore.getState();
    s.logActivity("info", "first");
    s.logActivity("success", "second");
    s.logActivity("error", "third");
    const { activity } = useUiStore.getState();
    expect(activity.map((e) => e.text)).toEqual(["first", "second", "third"]);
    expect(activity.map((e) => e.level)).toEqual(["info", "success", "error"]);
    // Each entry carries a unique id and a numeric timestamp.
    expect(new Set(activity.map((e) => e.id)).size).toBe(3);
    for (const e of activity) expect(typeof e.ts).toBe("number");
  });

  it("bounds the activity log to ACTIVITY_LOG_LIMIT, dropping the oldest", () => {
    const s = useUiStore.getState();
    for (let i = 0; i < ACTIVITY_LOG_LIMIT + 5; i += 1) {
      s.logActivity("info", `line ${i}`);
    }
    const { activity } = useUiStore.getState();
    expect(activity).toHaveLength(ACTIVITY_LOG_LIMIT);
    // The oldest five were dropped; the newest line is last.
    expect(activity[0]?.text).toBe("line 5");
    expect(activity[activity.length - 1]?.text).toBe(
      `line ${ACTIVITY_LOG_LIMIT + 4}`,
    );
  });
});
