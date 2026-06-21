// In-memory Tauri mock (DESIGN.md §14.4–§14.5).
//
// Two consumers share this one in-memory backend:
//
//   1. TESTS. There is no Tauri runtime under Vitest, so
//      `@tauri-apps/api/core::invoke` and `@tauri-apps/api/event::listen` are
//      replaced (in src/test/setup.ts) by the `mockInvoke` / `mockListen` here. A
//      test seeds a fixture GraphView + canned command replies, then drives the
//      UI; to exercise the reducer/optimistic path it calls `emitOpLogEvent` /
//      `emitEphemeral` to replay events the real backend would forward.
//
//   2. BROWSER MOCK MODE. A plain browser (the Vite dev server at
//      localhost:1420) has no Tauri runtime, so `src/ipc/client.ts` routes every
//      call here instead of `invoke`/`listen` (so the app runs fully, no throw).
//      In that mode `seedDemoGraph()` seeds a small demo DAG so the canvas
//      renders, and `autoEmit` is turned on so a `dispatch` mutation also EMITS
//      its plausible op-log event(s) — exactly what the real daemon forwards —
//      so the reducer folds them, the canvas updates, and the optimistic op
//      reconciles. Tests leave `autoEmit` off (they replay events explicitly),
//      so their behavior is unchanged.
//
// This is the mockable IPC the spec requires: it returns fixture graph views and
// replays events with no real daemon and no display.

import type {
  Command,
  CommandResult,
  EdgeType,
  EphemeralFrame,
  GraphView,
  NodeView,
  OpLogEvent,
  TauriEvent,
  Ulid,
} from "./types";
import { OPLOG_EVENT, EPHEMERAL_EVENT } from "./types";

/** A registered Tauri-style event listener with its unlisten handle. */
interface Listener<T> {
  handler: (event: TauriEvent<T>) => void;
}

interface MockState {
  /** The GraphView returned by the `graph_view` command. */
  graphView: GraphView;
  /** Recorded `dispatch` payloads, in call order (for assertions). */
  dispatched: Command[];
  /** Recorded `open_project` paths, in call order. */
  openedProjects: string[];
  /** Per-command-tag canned replies for `dispatch`. */
  dispatchReplies: Partial<Record<Command["command"], CommandResult>>;
  /**
   * Per-command-tag errors for `dispatch`: when set, dispatching that command
   * REJECTS with this error instead of replying — modeling a denied capability
   * (e.g. NetConnect-gated Push) or a real backend failure, so the UI's
   * error-surfacing path can be exercised.
   */
  dispatchErrors: Partial<Record<Command["command"], Error>>;
  /** A fallback reply factory when no per-tag reply is registered. */
  defaultDispatchReply: (cmd: Command) => CommandResult;
  /** Active listeners by Tauri event name. */
  listeners: Map<string, Set<Listener<unknown>>>;
  /** Monotonic id handed to each listener (mirrors Tauri's listener id). */
  nextListenerId: number;
  /**
   * Browser-mock-mode flag: when true, a `dispatch` mutation also emits its
   * plausible op-log event(s) onto the listeners so the reducer + optimistic UI
   * advance with no real daemon. Tests leave this `false` (they replay events
   * explicitly via `emitOpLogEvent`) so their behavior is unchanged.
   */
  autoEmit: boolean;
  /** A monotonic `seq` for auto-emitted op-log events (browser mode). */
  nextSeq: number;
}

const EMPTY_GRAPH: GraphView = {
  schemaVersion: 1,
  nodes: [],
  edges: [],
  refs: [],
};

function freshState(): MockState {
  return {
    graphView: structuredClone(EMPTY_GRAPH),
    dispatched: [],
    openedProjects: [],
    dispatchReplies: {},
    dispatchErrors: {},
    defaultDispatchReply: (cmd) => defaultReplyFor(cmd),
    listeners: new Map(),
    nextListenerId: 1,
    autoEmit: false,
    nextSeq: 1,
  };
}

let state: MockState = freshState();

/** A reasonable default reply per command kind, honoring the IPC rule. */
function defaultReplyFor(cmd: Command): CommandResult {
  switch (cmd.command) {
    case "NODE_DIFF":
      // In browser-mock mode, demo nodes get a plausible changed-path set so the
      // Changes tab shows something; tests (autoEmit off) keep the empty default.
      return {
        result: "DIFF",
        changedPaths: state.autoEmit ? demoChangedPaths() : [],
      };
    case "BLOB_READ":
      // Browser mock: return demo file content that VARIES by tree hash, so a
      // parent-tree-vs-node-tree diff renders a real before/after. Tests get [].
      return {
        result: "BLOB",
        bytes: state.autoEmit ? demoBlobBytes(cmd.treeHash, cmd.path) : [],
      };
    case "GC_RUN":
      return { result: "GC", reclaimable: [], bytes: 0 };
    case "GIT_EXPORT":
      // Action-shaped: an inline `Git` reply, no events (DESIGN.md §10.4).
      return {
        result: "GIT",
        branch: `spork/${cmd.nodeId}`,
        commitSha: fakeSha(),
        pushed: false,
      };
    case "GIT_PUSH":
      return {
        result: "GIT",
        branch: `spork/${cmd.nodeId}`,
        commitSha: fakeSha(),
        pushed: true,
      };
    case "NODE_AGENT_RUN": {
      // Mirror the daemon's reply: a mutation whose `ids` carry the resolved
      // provider/model + the priced cost, so the UI shows model + cost
      // immediately (the minimal op-log events don't carry them).
      const provider = cmd.modelKey.includes("/")
        ? cmd.modelKey.split("/")[0]
        : "anthropic";
      const cost = mockAgentCost(cmd.modelKey);
      return {
        result: "MUTATION",
        opId: fakeUlid(),
        ids: {
          nodeId: fakeUlid(),
          provider,
          model: cmd.modelKey || "anthropic/claude-sonnet-4-6",
          costMicroUsd: cost.microUsd,
          inputTokens: cost.inputTokens,
          outputTokens: cost.outputTokens,
          fallbacks: 0,
          intent: cmd.intent,
        },
      };
    }
    default:
      // Every other command is a mutation: reply with an op_id + minted ids.
      return { result: "MUTATION", opId: fakeUlid(), ids: mintedIdsFor(cmd) };
  }
}

/**
 * A plausible priced cost for a mock agent run, keyed by the model. Cloud models
 * cost something; a local server / CLI agent is free — matching the daemon's
 * built-in pricing so the browser demo's cost ledger is representative.
 */
function mockAgentCost(modelKey: string): {
  inputTokens: number;
  outputTokens: number;
  microUsd: number;
} {
  const inputTokens = 1200;
  const outputTokens = 300;
  let microUsd = 0;
  if (modelKey.includes("gpt-4o")) microUsd = 1200 * 2.5 + 300 * 10; // $2.5/$10 per Mtok → micro
  else if (modelKey.includes("claude") || modelKey === "") microUsd = 1200 * 3 + 300 * 15;
  // local/* and cli/* stay free (microUsd = 0).
  return { inputTokens, outputTokens, microUsd };
}

/**
 * The freshly-minted ids a mutation reply carries, so the optimistic-UI
 * reconciliation has a subject id to match a tailing op-log event against. A node
 * mutation mints a `nodeId`; a ref mutation mints a `refId`. The same minted id
 * is reused by the auto-emitted event in browser mode (see `autoEmitFor`).
 */
function mintedIdsFor(cmd: Command): Record<string, unknown> {
  switch (cmd.command) {
    case "NODE_CREATE":
    case "NODE_RESTORE":
    case "NODE_RUN_CHECK":
    case "BRANCH_MERGE":
      return { nodeId: fakeUlid() };
    case "BRANCH_FORK":
      return { refId: cmd.name };
    case "REF_CREATE":
    case "REF_MOVE":
      return { refId: cmd.name };
    default:
      return {};
  }
}

/**
 * The observing node kind a `NODE_RUN_CHECK` spec describes. The spec is an
 * opaque `unknown` on the command (the frozen contract carries no shape), so we
 * read its `kind` defensively and default to "validation".
 */
function checkSpecKind(spec: unknown): "validation" | "stress" | "sanity" {
  if (spec !== null && typeof spec === "object") {
    const kind = (spec as Record<string, unknown>)["kind"];
    if (kind === "stress" || kind === "sanity" || kind === "validation") {
      return kind;
    }
  }
  return "validation";
}

/** The typed edge that attaches an observing result of the given check kind. */
function edgeForCheckKind(
  kind: "validation" | "stress" | "sanity",
): EdgeType {
  switch (kind) {
    case "stress":
      return "STRESSES";
    case "sanity":
      return "CHECKS";
    case "validation":
      return "VALIDATES";
  }
}

/**
 * The op-log events the real daemon would forward for a mutation, reusing the
 * reply's minted subject id so the reducer folds the right node/ref and the
 * optimistic op reconciles. Returns `[]` for reads and the action-shaped git
 * commands (which emit nothing). Browser-mock-mode only.
 */
function autoEmitFor(cmd: Command, reply: CommandResult): OpLogEvent[] {
  if (reply.result !== "MUTATION") return [];
  const ids = reply.ids;
  const nodeId =
    typeof ids["nodeId"] === "string" ? (ids["nodeId"] as Ulid) : null;
  const refId =
    typeof ids["refId"] === "string" ? (ids["refId"] as Ulid) : null;

  const events: OpLogEvent[] = [];
  const seq = () => state.nextSeq++;
  switch (cmd.command) {
    case "NODE_CREATE":
      if (nodeId) {
        events.push({
          type: "NODE_CREATED",
          seq: seq(),
          nodeId,
          schemaVersion: 1,
        });
        for (const parent of cmd.parentIds) {
          events.push({
            type: "EDGE_ADDED",
            seq: seq(),
            from: parent,
            to: nodeId,
            edge: "PARENT_CHILD",
          });
        }
      }
      break;
    case "NODE_RESTORE":
      events.push({
        type: "RESTORE_PERFORMED",
        seq: seq(),
        nodeId: cmd.nodeId,
      });
      events.push({
        type: "REF_MOVED",
        seq: seq(),
        ref: "HEAD",
        to: cmd.nodeId,
      });
      break;
    case "BRANCH_FORK":
      if (refId) {
        events.push({ type: "BRANCH_FORKED", seq: seq(), ref: refId });
        events.push({ type: "REF_CREATED", seq: seq(), ref: refId });
      }
      break;
    case "REF_CREATE":
      if (refId) events.push({ type: "REF_CREATED", seq: seq(), ref: refId });
      break;
    case "REF_MOVE":
      if (refId)
        events.push({
          type: "REF_MOVED",
          seq: seq(),
          ref: refId,
          to: cmd.to,
        });
      break;
    case "NODE_RUN_CHECK":
      if (nodeId) {
        // The check kind (validation/stress/sanity) drives the TYPED edge
        // (VALIDATES/STRESSES/CHECKS). The EDGE_ADDED is emitted FIRST so the
        // reducer mints the result node from the observing edge with the matching
        // observing kind — so the canvas icon/color matches the legend instead of
        // the neutral Snapshot placeholder. RESULT_RECORDED then marks it passed.
        const kind = checkSpecKind(cmd.spec);
        events.push({
          type: "EDGE_ADDED",
          seq: seq(),
          from: cmd.targetNodeId,
          to: nodeId,
          edge: edgeForCheckKind(kind),
        });
        events.push({
          type: "RESULT_RECORDED",
          seq: seq(),
          runId: fakeUlid(),
          nodeId,
        });
      }
      break;
    case "BRANCH_MERGE":
      if (nodeId)
        events.push({ type: "MERGE_PERFORMED", seq: seq(), nodeId });
      break;
    case "OP_UNDO":
      events.push({ type: "OP_UNDONE", seq: seq() });
      break;
    case "OP_REDO":
      events.push({ type: "OP_REDONE", seq: seq() });
      break;
    case "NODE_AGENT_RUN":
      // The attached context node + its dotted DERIVED_FROM edge are upserted by
      // the action from the authoritative reply (model + cost). Emit just the
      // NODE_CREATED so the optimistic op reconciles against the minted nodeId;
      // the edge is NOT emitted (the upsert added it, and a DERIVED_FROM edge is
      // an attachment, not a lineage link).
      if (nodeId) {
        events.push({
          type: "NODE_CREATED",
          seq: seq(),
          nodeId,
          schemaVersion: 1,
        });
      }
      break;
    default:
      break;
  }
  return events;
}

let ulidCounter = 0;
/** A deterministic, monotonically increasing fake ULID for test stability. */
export function fakeUlid(): string {
  ulidCounter += 1;
  return ulidCounter.toString(36).toUpperCase().padStart(26, "0");
}

let shaCounter = 0;
/** A deterministic fake 40-hex commit SHA for the `Git` result. */
export function fakeSha(): string {
  shaCounter += 1;
  return shaCounter.toString(16).padStart(40, "0");
}

/** Reset all mock state (called from `beforeEach`). */
export function resetTauriMock(): void {
  state = freshState();
  ulidCounter = 0;
  shaCounter = 0;
}

/** Seed the GraphView returned by the `graph_view` command. */
export function setMockGraphView(view: GraphView): void {
  state.graphView = view;
}

/** Register a canned reply for a specific command tag. */
export function setDispatchReply(
  tag: Command["command"],
  reply: CommandResult,
): void {
  state.dispatchReplies[tag] = reply;
}

/**
 * Register an error for a specific command tag: dispatching it will REJECT with
 * this error instead of replying. Models a denied capability (e.g. the
 * NetConnect-gated Push) or a backend failure so the UI's error-surfacing path
 * can be exercised in tests.
 */
export function setDispatchError(tag: Command["command"], error: Error): void {
  state.dispatchErrors[tag] = error;
}

/** The recorded list of dispatched commands, in call order. */
export function getDispatchedCommands(): readonly Command[] {
  return state.dispatched;
}

/** The recorded list of opened project paths, in call order. */
export function getOpenedProjects(): readonly string[] {
  return state.openedProjects;
}

/** Whether the mock is in browser-mock auto-emit mode. */
export function isAutoEmit(): boolean {
  return state.autoEmit;
}

/** Replay an op-log event onto every `oplog-event` listener. */
export function emitOpLogEvent(event: OpLogEvent): void {
  deliver(OPLOG_EVENT, event);
}

/** Replay an ephemeral frame onto every `ephemeral` listener. */
export function emitEphemeral(frame: EphemeralFrame): void {
  deliver(EPHEMERAL_EVENT, frame);
}

function deliver<T>(eventName: string, payload: T): void {
  const set = state.listeners.get(eventName);
  if (!set) return;
  const id = state.nextListenerId;
  // Copy the set first: a handler may unlisten (mutating the set) while we
  // iterate, which would otherwise throw / skip.
  for (const l of [...set]) {
    (l as Listener<T>).handler({ event: eventName, id, payload });
  }
}

// --- The demo DAG (browser mock mode) ----------------------------------------

const DEMO_ROOT = "01DEMO00000000000000000ROOT";
const DEMO_EDIT = "01DEMO00000000000000000EDIT";
const DEMO_VALD = "01DEMO000000000000000VALIDAT";
const DEMO_SNAP = "01DEMO00000000000000000SNAP2";

function demoNode(
  id: Ulid,
  kind: string,
  family: NodeView["family"],
  status: NodeView["status"],
  ownsSnapshot: boolean,
  parentIds: Ulid[],
  model: string | null,
  cost: NodeView["cost"] = null,
): NodeView {
  return {
    id,
    kind,
    family,
    status,
    isStale: false,
    ownsSnapshot,
    snapshotHash: ownsSnapshot ? `b3:${id}` : null,
    branchId: "main",
    parentIds,
    model,
    cost,
  };
}

/**
 * A small demo DAG so the canvas renders something at localhost:1420: a root
 * snapshot, a mutating Edit child, an observing Validation off the Edit, and a
 * second Snapshot child — with parent/child edges, a VALIDATES edge, and a couple
 * of refs (HEAD + a branch). Used only in browser mock mode.
 */
export function demoGraphView(): GraphView {
  const nodes: NodeView[] = [
    demoNode(DEMO_ROOT, "snapshot", "mutating", "passed", true, [], "claude-sonnet-4-6"),
    demoNode(
      DEMO_EDIT,
      "codebase-edit",
      "mutating",
      "passed",
      true,
      [DEMO_ROOT],
      "claude-sonnet-4-6",
    ),
    demoNode(
      DEMO_VALD,
      "validation",
      "observing",
      "passed",
      false,
      [DEMO_EDIT],
      null,
    ),
    demoNode(
      DEMO_SNAP,
      "snapshot",
      "mutating",
      "pending",
      true,
      [DEMO_EDIT],
      "claude-sonnet-4-6",
    ),
  ];
  const edges = [
    { from: DEMO_ROOT, to: DEMO_EDIT, edgeType: "PARENT_CHILD" as EdgeType },
    { from: DEMO_EDIT, to: DEMO_VALD, edgeType: "VALIDATES" as EdgeType },
    { from: DEMO_EDIT, to: DEMO_SNAP, edgeType: "PARENT_CHILD" as EdgeType },
  ];
  const refs = [
    { name: "HEAD", kind: "Head" as const, target: DEMO_EDIT },
    { name: "main", kind: "Branch" as const, target: DEMO_EDIT },
  ];
  return { schemaVersion: 1, nodes, edges, refs };
}

/** A plausible changed-path set for any demo node (browser-mock mode). */
function demoChangedPaths(): string[] {
  return ["src/auth/login.ts", "src/auth/index.ts", "docs/README.md"];
}

/**
 * Demo file content for a `BLOB_READ`, varied by tree hash so the Changes tab's
 * parent-tree-vs-node-tree comparison renders a real before/after diff. The
 * `EDIT` tree adds a guard the `ROOT` tree lacks. Browser-mock mode only.
 */
function demoBlobBytes(treeHash: string, path: string): number[] {
  const guarded = treeHash.includes("EDIT") || treeHash.includes("SNAP");
  const body = guarded
    ? "  if (!value) throw new Error('empty');\n  return normalize(value);"
    : "  return normalize(value);";
  const text = `// ${path}\nexport function check(value: string) {\n${body}\n}\n`;
  return Array.from(new TextEncoder().encode(text));
}

/**
 * Enter browser mock mode: seed the demo DAG as the `graph_view` snapshot and
 * turn on auto-emit so dispatched mutations forward their op-log events. Called
 * once from `client.ts` when no Tauri runtime is present.
 */
export function seedDemoGraph(): void {
  state.graphView = demoGraphView();
  state.autoEmit = true;
  state.nextSeq = 1;
}

// --- The mocked @tauri-apps/api surface --------------------------------------

/** Mock of `@tauri-apps/api/core::invoke`. */
export async function mockInvoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  switch (command) {
    case "ping":
      return "pong" as unknown as T;
    case "open_project": {
      const path = (args?.["path"] as string) ?? "";
      state.openedProjects.push(path);
      return undefined as unknown as T;
    }
    case "graph_view":
      return structuredClone(state.graphView) as unknown as T;
    case "dispatch": {
      const cmd = args?.["command"] as Command;
      state.dispatched.push(cmd);
      // A registered error models a denied capability / backend failure: reject
      // so the caller's error-surfacing path runs (the real `invoke` rejects too).
      const err = state.dispatchErrors[cmd.command];
      if (err) throw err;
      const reply =
        state.dispatchReplies[cmd.command] ?? state.defaultDispatchReply(cmd);
      // In browser mock mode, forward the plausible op-log events the real
      // daemon would emit AFTER returning the reply, so the reducer + optimistic
      // UI advance. Deliver on a microtask so the caller has registered its
      // optimistic op (beginOptimistic) before the reconciling event lands.
      if (state.autoEmit) {
        const events = autoEmitFor(cmd, reply);
        if (events.length > 0) {
          void Promise.resolve().then(() => {
            for (const ev of events) emitOpLogEvent(ev);
          });
        }
      }
      return reply as unknown as T;
    }
    default:
      throw new Error(`mockInvoke: unhandled command "${command}"`);
  }
}

/** Mock of `@tauri-apps/api/event::listen`. Returns an unlisten function. */
export async function mockListen<T>(
  eventName: string,
  handler: (event: TauriEvent<T>) => void,
): Promise<() => void> {
  let set = state.listeners.get(eventName);
  if (!set) {
    set = new Set();
    state.listeners.set(eventName, set);
  }
  const listener: Listener<unknown> = {
    handler: handler as Listener<unknown>["handler"],
  };
  set.add(listener);
  return () => {
    set?.delete(listener);
  };
}
