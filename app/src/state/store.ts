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
import type {
  EphemeralFrame,
  GraphView,
  NodeView,
  OpLogEvent,
  Ulid,
} from "../ipc/types";
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

/** The severity of an activity-log entry (drives the rail's per-line styling). */
export type ActivityLevel = "info" | "success" | "error";

/**
 * One human-readable line in the activity log — the outcome (or error) of a
 * toolbar action. Surfaced in the bottom Run-output rail so every action gives
 * the user feedback, even the ones that create no canvas node.
 */
export interface ActivityEntry {
  /** A stable id for the React key. */
  id: string;
  /** When the entry was appended (epoch ms). */
  ts: number;
  /** Severity, driving the per-line style (muted / green / red). */
  level: ActivityLevel;
  /** The human-readable outcome line. */
  text: string;
}

/** How many activity entries the bounded log retains (oldest dropped first). */
export const ACTIVITY_LOG_LIMIT = 100;

/** Daemon connection state, shown by the top-bar dot + status sub-strip (§5.12). */
export type ConnectionState =
  | "connected"
  | "reconnecting"
  | "disconnected"
  // First-run: the daemon is alive but no project is open yet (graph_view
  // returns "no project open"). Distinct from a real disconnection so onboarding
  // shows instead of an alarming "Lost connection" banner.
  | "no-project"
  | "mock";

/** Layout density (Settings → §5.13). */
export type DensityMode = "comfortable" | "compact";

/**
 * The modal currently open, if any (UI_UX_DESIGN.md §5.10/§5.13). A discriminated
 * union so any component can request a modal and the Shell renders exactly one.
 * Every variant maps to a BUILT (🟢) command or local action — no forward-map
 * surface appears here.
 */
export type AppModal =
  | { kind: "newBranch"; nodeId: Ulid }
  | { kind: "merge"; nodeId: Ulid }
  | { kind: "restore"; nodeId: Ulid }
  | { kind: "commit"; nodeId: Ulid }
  | { kind: "push"; nodeId: Ulid }
  | { kind: "gc" }
  | { kind: "settings" }
  /** Ask the agent about a node (P6 read-only run → attached context node). */
  | { kind: "askAgent"; nodeId: Ulid }
  /** A denied capability surfaced honestly (§5.10e); inline grant is forward-map. */
  | { kind: "capability"; capability: string; action: string };

/** An open context menu anchored at a screen point (§5.11). */
export type ContextMenu =
  | { kind: "node"; nodeId: Ulid; x: number; y: number }
  | { kind: "branch"; ref: string; target: Ulid; x: number; y: number };

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
  /**
   * A bounded, newest-last activity log of action outcomes + errors, rendered in
   * the bottom Run-output rail so every toolbar action gives the user feedback —
   * including the ones (git/branch/restore) that create no canvas node.
   */
  activity: ActivityEntry[];
  /** The highest op-log `seq` folded so far (gap detection). */
  lastSeq: number;

  // --- chrome / layout UI state (UI_UX_DESIGN.md §4, §12) ---
  /** Daemon connection state for the status dot / sub-strip + banner. */
  connection: ConnectionState;
  /** Left navigator collapsed to a thin rail. */
  navCollapsed: boolean;
  /** Right details panel collapsed. */
  detailsCollapsed: boolean;
  /** Bottom rail collapsed to its tab bar. */
  railCollapsed: boolean;
  /** Layout density (applies `data-density` on the document element). */
  density: DensityMode;
  /**
   * Node-type filter: the set of kinds to HIGHLIGHT. Empty = no filter (all
   * normal). When non-empty, non-matching nodes dim on the canvas (§5.2/§7.2).
   */
  kindFilter: string[];
  /** Canvas free-text search query; non-matching nodes dim (§5.4/§7.3). */
  canvasSearch: string;
  /** The currently open modal, or null. */
  modal: AppModal | null;
  /** Whether the ⌘K command palette is open. */
  paletteOpen: boolean;
  /** The open context menu, or null. */
  contextMenu: ContextMenu | null;
  /** Whether the settings popover is open. */
  settingsOpen: boolean;

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
  /**
   * Upsert an agent-run's attached context node and its dotted `DERIVED_FROM`
   * edge to `targetId`, from the authoritative `NODE_AGENT_RUN` reply (P6). The
   * reply's `ids` carry the model + cost the minimal op-log events do not, so the
   * cost/model show immediately rather than only after a full graph_view resync.
   */
  attachAgentNode: (node: NodeView, targetId: Ulid) => void;
  /** Buffer one ephemeral frame onto the node's rail (non-blocking). */
  ingestEphemeral: (frame: EphemeralFrame) => void;
  /**
   * Append one human-readable entry to the bounded activity log (newest last).
   * The log is trimmed to `ACTIVITY_LOG_LIMIT` so it never grows without bound.
   */
  logActivity: (level: ActivityLevel, text: string) => void;
  /** Set the daemon connection state. */
  setConnection: (state: ConnectionState) => void;
  /** Toggle the left navigator collapsed state. */
  toggleNav: () => void;
  /** Toggle the right details panel collapsed state. */
  toggleDetails: () => void;
  /** Set the bottom rail collapsed state. */
  setRailCollapsed: (collapsed: boolean) => void;
  /** Set layout density. */
  setDensity: (density: DensityMode) => void;
  /** Toggle a node `kind` in the highlight filter (empty = no filter). */
  toggleKindFilter: (kind: string) => void;
  /** Clear the node-type filter. */
  clearKindFilter: () => void;
  /** Set the canvas search query. */
  setCanvasSearch: (q: string) => void;
  /** Open a modal (replaces any open modal). */
  openModal: (modal: AppModal) => void;
  /** Close the open modal. */
  closeModal: () => void;
  /** Open/close the ⌘K command palette. */
  setPaletteOpen: (open: boolean) => void;
  /** Open a context menu (replaces any open one). */
  openContextMenu: (menu: ContextMenu) => void;
  /** Close the open context menu. */
  closeContextMenu: () => void;
  /** Open/close the settings popover. */
  setSettingsOpen: (open: boolean) => void;
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
  // The default model selector (a `provider/model` key); the top-bar selector
  // defaults to this and it is one of TopBar's MODELS, and an agent run uses it
  // as its selector (DESIGN.md §12.3, §14.2).
  defaultModel: "anthropic/claude-sonnet-4-6",
  pending: {} as Record<Ulid, PendingOp>,
  rail: {} as RunRail,
  activity: [] as ActivityEntry[],
  lastSeq: 0,
  connection: "connected" as ConnectionState,
  navCollapsed: false,
  detailsCollapsed: false,
  railCollapsed: false,
  density: "comfortable" as DensityMode,
  kindFilter: [] as string[],
  canvasSearch: "",
  modal: null as AppModal | null,
  paletteOpen: false,
  contextMenu: null as ContextMenu | null,
  settingsOpen: false,
};

/** A monotonic counter making each activity entry's React key unique. */
let activitySeq = 0;

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

  attachAgentNode: (node, targetId) =>
    set((s) => {
      const nodes = s.view.nodes.some((n) => n.id === node.id)
        ? s.view.nodes.map((n) => (n.id === node.id ? node : n))
        : [...s.view.nodes, node];
      const hasEdge = s.view.edges.some(
        (e) =>
          e.from === node.id &&
          e.to === targetId &&
          e.edgeType === "DERIVED_FROM",
      );
      const edges = hasEdge
        ? s.view.edges
        : [
            ...s.view.edges,
            { from: node.id, to: targetId, edgeType: "DERIVED_FROM" as const },
          ];
      return { view: { ...s.view, nodes, edges } };
    }),

  ingestEphemeral: (frame) =>
    set((s) => {
      const prev = s.rail[frame.nodeId] ?? [];
      return {
        rail: { ...s.rail, [frame.nodeId]: [...prev, frame.data] },
      };
    }),

  logActivity: (level, text) =>
    set((s) => {
      activitySeq += 1;
      const entry: ActivityEntry = {
        id: `act-${activitySeq}`,
        ts: Date.now(),
        level,
        text,
      };
      // Append newest-last, then trim the oldest beyond the bound.
      const next = [...s.activity, entry];
      return {
        activity:
          next.length > ACTIVITY_LOG_LIMIT
            ? next.slice(next.length - ACTIVITY_LOG_LIMIT)
            : next,
      };
    }),

  setConnection: (connection) => set({ connection }),

  toggleNav: () => set((s) => ({ navCollapsed: !s.navCollapsed })),

  toggleDetails: () => set((s) => ({ detailsCollapsed: !s.detailsCollapsed })),

  setRailCollapsed: (railCollapsed) => set({ railCollapsed }),

  setDensity: (density) => set({ density }),

  toggleKindFilter: (kind) =>
    set((s) => ({
      kindFilter: s.kindFilter.includes(kind)
        ? s.kindFilter.filter((k) => k !== kind)
        : [...s.kindFilter, kind],
    })),

  clearKindFilter: () => set({ kindFilter: [] }),

  setCanvasSearch: (canvasSearch) => set({ canvasSearch }),

  openModal: (modal) => set({ modal, contextMenu: null, paletteOpen: false }),

  closeModal: () => set({ modal: null }),

  setPaletteOpen: (paletteOpen) => set({ paletteOpen }),

  openContextMenu: (contextMenu) => set({ contextMenu }),

  closeContextMenu: () => set({ contextMenu: null }),

  setSettingsOpen: (settingsOpen) => set({ settingsOpen }),

  reset: () =>
    set({
      ...INITIAL,
      pending: {},
      rail: {},
      activity: [],
      kindFilter: [],
      modal: null,
      contextMenu: null,
    }),
}));
