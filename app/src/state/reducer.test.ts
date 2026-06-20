// Reducer tests (DESIGN.md §14.4) — the pure op-log fold.

import { describe, it, expect } from "vitest";
import { applyOpLogEvent, reduceStream, EMPTY_VIEW } from "./reducer";
import type { OpLogEvent } from "../ipc/types";

const A = "00000000000000000000000001";
const B = "00000000000000000000000002";
const RUN = "00000000000000000000000099";

describe("applyOpLogEvent", () => {
  it("folds NODE_CREATED into a new node", () => {
    const next = applyOpLogEvent(EMPTY_VIEW, {
      type: "NODE_CREATED",
      seq: 1,
      nodeId: A,
      schemaVersion: 1,
    });
    expect(next.nodes).toHaveLength(1);
    expect(next.nodes[0]?.id).toBe(A);
    // Purity: the input was not mutated.
    expect(EMPTY_VIEW.nodes).toHaveLength(0);
  });

  it("folds EDGE_ADDED, creating endpoints and recording the parent link", () => {
    const created = reduceStream(EMPTY_VIEW, [
      { type: "NODE_CREATED", seq: 1, nodeId: A, schemaVersion: 1 },
      { type: "NODE_CREATED", seq: 2, nodeId: B, schemaVersion: 1 },
      { type: "EDGE_ADDED", seq: 3, from: A, to: B, edge: "PARENT_CHILD" },
    ]);
    expect(created.edges).toEqual([
      { from: A, to: B, edgeType: "PARENT_CHILD" },
    ]);
    expect(created.nodes.find((n) => n.id === B)?.parentIds).toEqual([A]);
  });

  it("is idempotent on a duplicate EDGE_ADDED", () => {
    const e: OpLogEvent = {
      type: "EDGE_ADDED",
      seq: 3,
      from: A,
      to: B,
      edge: "PARENT_CHILD",
    };
    const once = applyOpLogEvent(EMPTY_VIEW, e);
    const twice = applyOpLogEvent(once, e);
    expect(twice.edges).toHaveLength(1);
    expect(twice.nodes.find((n) => n.id === B)?.parentIds).toEqual([A]);
  });

  it("folds REF_MOVED into a ref upsert", () => {
    const next = applyOpLogEvent(EMPTY_VIEW, {
      type: "REF_MOVED",
      seq: 1,
      ref: "HEAD",
      to: B,
    });
    expect(next.refs).toEqual([{ name: "HEAD", kind: "Head", target: B }]);
  });

  it("folds RESTORE_PERFORMED by moving HEAD to the restored node", () => {
    const next = applyOpLogEvent(EMPTY_VIEW, {
      type: "RESTORE_PERFORMED",
      seq: 1,
      nodeId: A,
    });
    expect(next.refs.find((r) => r.name === "HEAD")?.target).toBe(A);
  });

  it("folds RESULT_RECORDED into an observing node marked passed", () => {
    const next = applyOpLogEvent(EMPTY_VIEW, {
      type: "RESULT_RECORDED",
      seq: 1,
      runId: RUN,
      nodeId: A,
    });
    const node = next.nodes.find((n) => n.id === A);
    expect(node?.family).toBe("observing");
    expect(node?.status).toBe("passed");
  });

  it("mints an observing result node of the right kind from a check edge", () => {
    // A NODE_RUN_CHECK forwards EDGE_ADDED (the observing edge) then
    // RESULT_RECORDED. The result node must render with the matching observing
    // kind (icon/color = legend), not the neutral Snapshot placeholder.
    const cases: { edge: OpLogEvent; kind: string }[] = [
      {
        edge: { type: "EDGE_ADDED", seq: 1, from: A, to: B, edge: "VALIDATES" },
        kind: "validation",
      },
      {
        edge: { type: "EDGE_ADDED", seq: 1, from: A, to: B, edge: "STRESSES" },
        kind: "stress",
      },
      {
        edge: { type: "EDGE_ADDED", seq: 1, from: A, to: B, edge: "CHECKS" },
        kind: "sanity",
      },
    ];
    for (const { edge, kind } of cases) {
      const view = reduceStream(EMPTY_VIEW, [
        edge,
        { type: "RESULT_RECORDED", seq: 2, runId: RUN, nodeId: B },
      ]);
      const node = view.nodes.find((n) => n.id === B);
      expect(node?.kind).toBe(kind);
      expect(node?.family).toBe("observing");
      expect(node?.status).toBe("passed");
    }
  });

  it("keeps a structural (PARENT_CHILD) edge's placeholder a snapshot", () => {
    const view = applyOpLogEvent(EMPTY_VIEW, {
      type: "EDGE_ADDED",
      seq: 1,
      from: A,
      to: B,
      edge: "PARENT_CHILD",
    });
    expect(view.nodes.find((n) => n.id === B)?.kind).toBe("snapshot");
  });

  it("ignores an unknown future event variant (forward-tolerant)", () => {
    // A NEWER daemon emits a variant this build has never seen. The reducer must
    // return the fold unchanged, never throw (CLAUDE.md C2/C5).
    const future = {
      type: "POLICY_EVALUATED",
      seq: 7,
      policyRef: "main",
    } as unknown as OpLogEvent;
    const seeded = applyOpLogEvent(EMPTY_VIEW, {
      type: "NODE_CREATED",
      seq: 1,
      nodeId: A,
      schemaVersion: 1,
    });
    const next = applyOpLogEvent(seeded, future);
    expect(next).toBe(seeded); // unchanged reference: nothing folded
  });

  it("reduces a full stream deterministically", () => {
    const view = reduceStream(EMPTY_VIEW, [
      { type: "NODE_CREATED", seq: 1, nodeId: A, schemaVersion: 1 },
      { type: "NODE_CREATED", seq: 2, nodeId: B, schemaVersion: 1 },
      { type: "EDGE_ADDED", seq: 3, from: A, to: B, edge: "PARENT_CHILD" },
      { type: "REF_MOVED", seq: 4, ref: "HEAD", to: B },
      { type: "GC_PERFORMED", seq: 5 },
    ]);
    expect(view.nodes).toHaveLength(2);
    expect(view.edges).toHaveLength(1);
    expect(view.refs).toHaveLength(1);
  });
});
