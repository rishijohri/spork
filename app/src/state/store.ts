// The UI state store (DESIGN.md §14.4).
//
// Zustand holds the view-model (nodes/edges/refs), the current selection, the
// default model for new nodes (top-bar selector), and the optimistic-mutation
// bookkeeping keyed by `opId`. Daemon-backed READS (graph_view) are owned by
// React Query (see src/state/queries.ts); this store owns the live, reduced
// view-model and ephemeral UI concerns.
//
// Optimistic UI (DESIGN.md §14.4): a mutation registers an optimistic op via
// `beginOptimistic(opId, label, subjectId)`, recording the freshly-minted
// subject id (the new `nodeId`/`refId`) the daemon returned. The matching op-log
// events tail in over the stream and are folded by `ingestEvent`, which also
// resolves the optimistic op once it observes an event referencing that subject
// id — `OpLogEvent` carries no `opId` (the frozen contract, A.1), so subject id
// is the correlation key. The single reconciliation path is the ordered event
// stream — never the mutation's return value.

import { create } from "zustand";
import type { EphemeralFrame, GraphView, OpLogEvent, Ulid } from "../ipc/types";
import { applyOpLogEvent, EMPTY_VIEW } from "./reducer";

/** A pending optimistic mutation awaiting its op-log reconciliation. */
export interface PendingOp {
  opId: Ulid;
  /** What the mutation was, for debugging / rollback messaging. */
  label: string;
  startedAt: number;
  /**
   * The freshly-minted subject id the daemon returned in the `MUTATION` result's
   * `ids` bag (the new `nodeId`, or `refId` for ref ops). Since `OpLogEvent`
   * carries no `opId` (the frozen contract, A.1), reconciliation matches a
   * tailing event's subject against this minted id — the single reconciliation
   * path. `null` when the mutation minted no addressable subject.
   */
  subjectId: Ulid | null;
}

/** The bottom run-rail line buffer, keyed by node id. */
export type RunRail = Record<Ulid, string[]>;

export interface UiState {
  /** The live, reduced view-model. */
  view: GraphView;
  /** The currently selected node id, if any. */
  selectedNodeId: Ulid | null;
  /** The default model the top-bar selector applies to new nodes. */
  defaultModel: string;
  /** In-flight optimistic mutations by opId. */
  pending: Record<Ulid, PendingOp>;
  /** Buffered ephemeral run/chat output per node (bottom rail / chat tab). */
  rail: RunRail;
  /** The highest op-log `seq` folded so far (gap detection). */
  lastSeq: number;

  // --- actions ---
  /** Replace the whole view-model (e.g. after a `graph_view` fetch). */
  setView: (view: GraphView) => void;
  /** Set the selected node. */
  selectNode: (id: Ulid | null) => void;
  /** Set the default model for new nodes. */
  setDefaultModel: (model: string) => void;
  /**
   * Register an optimistic mutation awaiting reconciliation. `subjectId` is the
   * minted id (the new `nodeId`/`refId`) the tailing op-log event will reference;
   * pass `null` when the mutation minted no addressable subject.
   */
  beginOptimistic: (opId: Ulid, label: string, subjectId?: Ulid | null) => void;
  /** Resolve (clear) an optimistic mutation. */
  resolveOptimistic: (opId: Ulid) => void;
  /** Fold one op-log event into the view-model and reconcile optimistic ops. */
  ingestEvent: (event: OpLogEvent) => void;
  /** Buffer one ephemeral frame onto the node's rail (non-blocking). */
  ingestEphemeral: (frame: EphemeralFrame) => void;
  /** Reset the store to its initial state (tests). */
  reset: () => void;
}

/**
 * The addressable subject id an op-log event references — the new/affected
 * `nodeId` for node events, or the ref name for ref events — used to reconcile a
 * pending optimistic op against the minted id it recorded. `null` for
 * bookkeeping-only events (undo/redo/gc/check-scheduled) that address no subject.
 */
function eventSubjectId(event: OpLogEvent): Ulid | null {
  switch (event.type) {
    case "NODE_CREATED":
    case "RESTORE_PERFORMED":
    case "RESULT_RECORDED":
    case "MERGE_PERFORMED":
      return event.nodeId;
    case "REF_CREATED":
    case "BRANCH_FORKED":
    case "REF_MOVED":
      return event.ref as Ulid;
    default:
      return null;
  }
}

const INITIAL = {
  view: EMPTY_VIEW,
  selectedNodeId: null as Ulid | null,
  // The default model for new nodes; the top-bar selector defaults to this and
  // it is one of TopBar's MODELS (DESIGN.md §14.2).
  defaultModel: "claude-sonnet-4-6",
  pending: {} as Record<Ulid, PendingOp>,
  rail: {} as RunRail,
  lastSeq: 0,
};

export const useUiStore = create<UiState>((set) => ({
  ...INITIAL,

  setView: (view) => set({ view }),

  selectNode: (id) => set({ selectedNodeId: id }),

  setDefaultModel: (model) => set({ defaultModel: model }),

  beginOptimistic: (opId, label, subjectId = null) =>
    set((s) => ({
      pending: {
        ...s.pending,
        [opId]: { opId, label, startedAt: Date.now(), subjectId },
      },
    })),

  resolveOptimistic: (opId) =>
    set((s) => {
      if (!s.pending[opId]) return s;
      const next = { ...s.pending };
      delete next[opId];
      return { pending: next };
    }),

  ingestEvent: (event) =>
    set((s) => {
      // The single reconciliation path (DESIGN.md §14.4): fold the durable event
      // into the view, then clear any optimistic op whose minted subject id the
      // event references. `OpLogEvent` carries no `opId` (frozen contract, A.1),
      // so we match on the subject id recorded at `beginOptimistic` time.
      const subject = eventSubjectId(event);
      let pending = s.pending;
      if (subject !== null) {
        const matched = Object.values(s.pending).filter(
          (p) => p.subjectId === subject,
        );
        if (matched.length > 0) {
          pending = { ...s.pending };
          for (const p of matched) delete pending[p.opId];
        }
      }
      return {
        view: applyOpLogEvent(s.view, event),
        lastSeq: Math.max(s.lastSeq, event.seq),
        pending,
      };
    }),

  ingestEphemeral: (frame) =>
    set((s) => {
      const prev = s.rail[frame.nodeId] ?? [];
      return {
        rail: { ...s.rail, [frame.nodeId]: [...prev, frame.data] },
      };
    }),

  reset: () =>
    set({
      ...INITIAL,
      pending: {},
      rail: {},
    }),
}));
