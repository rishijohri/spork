// The pure op-log reducer (DESIGN.md §14.4).
//
// `applyOpLogEvent(state, ev)` folds one ordered `OpLogEvent` into the
// view-model. It is PURE (no I/O, returns a new state, never mutates the input)
// so it is trivially testable and can replay a whole stream deterministically.
//
// Forward-tolerance (CLAUDE.md C2/C5): an event variant this build does not
// recognize is IGNORED — the fold is returned unchanged — exactly as the Rust
// `MaybeEvent` wrapper guarantees on the daemon side. Because `OpLogEvent` is a
// closed TS union, "unknown" means a runtime tag outside the union; the default
// switch arm handles it without throwing.

import type {
  EdgeView,
  GraphView,
  Lifecycle,
  NodeView,
  OpLogEvent,
  RefView,
  Ulid,
} from "../ipc/types";

/** An empty view-model, used as the reducer's seed. */
export const EMPTY_VIEW: GraphView = {
  schemaVersion: 1,
  nodes: [],
  edges: [],
  refs: [],
};

/** A minimal placeholder node created when an event references an unseen id. */
function placeholderNode(id: Ulid): NodeView {
  return {
    id,
    kind: "snapshot",
    family: "mutating",
    status: "pending",
    isStale: false,
    ownsSnapshot: false,
    snapshotHash: null,
    branchId: "main",
    parentIds: [],
    model: null,
  };
}

/**
 * The observing node kind implied by an observing edge type, or `null` for a
 * structural (non-observing) edge. An observing result node attached via one of
 * these edges renders with the matching legend icon/color (validation/stress/
 * sanity) instead of the neutral Snapshot placeholder.
 */
function observingKindForEdge(
  edge: EdgeView["edgeType"],
): { kind: string } | null {
  switch (edge) {
    case "VALIDATES":
      return { kind: "validation" };
    case "STRESSES":
      return { kind: "stress" };
    case "CHECKS":
      return { kind: "sanity" };
    default:
      return null;
  }
}

/** A placeholder observing result node of the given kind (for an observing edge). */
function observingResultNode(id: Ulid, kind: string): NodeView {
  return { ...placeholderNode(id), kind, family: "observing" };
}

function upsertEdge(edges: EdgeView[], edge: EdgeView): EdgeView[] {
  const exists = edges.some(
    (e) =>
      e.from === edge.from && e.to === edge.to && e.edgeType === edge.edgeType,
  );
  if (exists) return edges;
  return [...edges, edge];
}

function upsertRef(refs: RefView[], ref: RefView): RefView[] {
  const idx = refs.findIndex((r) => r.name === ref.name);
  if (idx === -1) return [...refs, ref];
  const next = refs.slice();
  next[idx] = ref;
  return next;
}

function setNodeStatus(
  nodes: NodeView[],
  id: Ulid,
  status: Lifecycle,
): NodeView[] {
  return nodes.map((n) => (n.id === id ? { ...n, status } : n));
}

/**
 * Fold one op-log event into the view-model, returning a new view.
 *
 * Handles the durable transitions the canvas needs: node creation, edge add, ref
 * move/create, restore, and observing-result recording. Bookkeeping-only events
 * (undo/redo/gc/check-scheduled/branch-forked/merge) pass through with their
 * minimal effect. Any tag outside the known union is ignored.
 */
export function applyOpLogEvent(
  state: GraphView,
  ev: OpLogEvent,
): GraphView {
  switch (ev.type) {
    case "NODE_CREATED": {
      if (state.nodes.some((n) => n.id === ev.nodeId)) return state;
      return {
        ...state,
        nodes: [...state.nodes, placeholderNode(ev.nodeId)],
      };
    }

    case "EDGE_ADDED": {
      // Ensure both endpoints exist (a NODE_CREATED may not have arrived yet in
      // a partial replay), then add the typed edge and record the parent link.
      let nodes = state.nodes;
      if (!nodes.some((n) => n.id === ev.from)) {
        nodes = [...nodes, placeholderNode(ev.from)];
      }
      if (!nodes.some((n) => n.id === ev.to)) {
        // If the edge is an observing check edge, the target is a result node:
        // mint it with the matching observing kind so it renders with the right
        // legend icon/color rather than the neutral Snapshot placeholder.
        const observing = observingKindForEdge(ev.edge);
        nodes = [
          ...nodes,
          observing
            ? observingResultNode(ev.to, observing.kind)
            : placeholderNode(ev.to),
        ];
      }
      nodes = nodes.map((n) =>
        n.id === ev.to && !n.parentIds.includes(ev.from)
          ? { ...n, parentIds: [...n.parentIds, ev.from] }
          : n,
      );
      return {
        ...state,
        nodes,
        edges: upsertEdge(state.edges, {
          from: ev.from,
          to: ev.to,
          edgeType: ev.edge,
        }),
      };
    }

    case "REF_MOVED":
      return {
        ...state,
        refs: upsertRef(state.refs, {
          name: ev.ref,
          kind: ev.ref === "HEAD" ? "Head" : "Branch",
          target: ev.to,
        }),
      };

    case "REF_CREATED":
    case "BRANCH_FORKED":
      // A create/fork event names the ref but not its target; record the ref so
      // the navigator lists it. The subsequent REF_MOVED (or a graph_view
      // refetch) supplies the target. Until then, point it at itself-less.
      if (state.refs.some((r) => r.name === ev.ref)) return state;
      return {
        ...state,
        refs: [
          ...state.refs,
          {
            name: ev.ref,
            kind: ev.ref === "HEAD" ? "Head" : "Branch",
            target: ev.ref as Ulid,
          },
        ],
      };

    case "RESTORE_PERFORMED":
      // Restore moves HEAD to the restored node; mark it as the current head.
      return {
        ...state,
        refs: upsertRef(state.refs, {
          name: "HEAD",
          kind: "Head",
          target: ev.nodeId,
        }),
      };

    case "RESULT_RECORDED": {
      // An observing result node was attached. Ensure it exists and mark it
      // passed (a recorded result is a completed observation).
      let nodes = state.nodes;
      if (!nodes.some((n) => n.id === ev.nodeId)) {
        nodes = [
          ...nodes,
          { ...placeholderNode(ev.nodeId), family: "observing", kind: "validation" },
        ];
      }
      return { ...state, nodes: setNodeStatus(nodes, ev.nodeId, "passed") };
    }

    case "CHECK_SCHEDULED":
    case "MERGE_PERFORMED":
    case "OP_UNDONE":
    case "OP_REDONE":
    case "GC_PERFORMED":
      // Bookkeeping-only for the view-model; no node/edge/ref change to fold.
      // (A real slice may refetch graph_view on these; the reducer stays pure.)
      return state;

    default:
      // Forward-tolerance: an unrecognized future variant is ignored so an old
      // reducer never crashes on a newer daemon (CLAUDE.md C2/C5).
      return state;
  }
}

/** Replay a whole ordered stream of events from a seed view (pure). */
export function reduceStream(
  seed: GraphView,
  events: readonly OpLogEvent[],
): GraphView {
  return events.reduce(applyOpLogEvent, seed);
}
