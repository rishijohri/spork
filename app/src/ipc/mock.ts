// In-memory Tauri mock for tests (DESIGN.md §14.4–§14.5).
//
// There is no Tauri runtime under Vitest, so `@tauri-apps/api/core::invoke` and
// `@tauri-apps/api/event::listen` are replaced (in src/test/setup.ts) by the
// `mockInvoke` / `mockListen` here. A test seeds a fixture GraphView + canned
// command replies, then drives the UI; to exercise the reducer/optimistic path
// it calls `emitOpLogEvent` / `emitEphemeral` to replay events the real backend
// would forward over the window channels.
//
// This is the mockable IPC the spec requires: it returns fixture graph views and
// replays events with no real daemon and no display.

import type {
  Command,
  CommandResult,
  EphemeralFrame,
  GraphView,
  OpLogEvent,
  TauriEvent,
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
  /** A fallback reply factory when no per-tag reply is registered. */
  defaultDispatchReply: (cmd: Command) => CommandResult;
  /** Active listeners by Tauri event name. */
  listeners: Map<string, Set<Listener<unknown>>>;
  /** Monotonic id handed to each listener (mirrors Tauri's listener id). */
  nextListenerId: number;
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
    defaultDispatchReply: (cmd) => defaultReplyFor(cmd),
    listeners: new Map(),
    nextListenerId: 1,
  };
}

let state: MockState = freshState();

/** A reasonable default reply per command kind, honoring the IPC rule. */
function defaultReplyFor(cmd: Command): CommandResult {
  switch (cmd.command) {
    case "NODE_DIFF":
      return { result: "DIFF", changedPaths: [] };
    case "BLOB_READ":
      return { result: "BLOB", bytes: [] };
    case "GC_RUN":
      return { result: "GC", reclaimable: [], bytes: 0 };
    default:
      // Every other command is a mutation: reply with an op_id + empty ids.
      return { result: "MUTATION", opId: fakeUlid(), ids: {} };
  }
}

let ulidCounter = 0;
/** A deterministic, monotonically increasing fake ULID for test stability. */
export function fakeUlid(): string {
  ulidCounter += 1;
  return ulidCounter.toString(36).toUpperCase().padStart(26, "0");
}

/** Reset all mock state (called from `beforeEach`). */
export function resetTauriMock(): void {
  state = freshState();
  ulidCounter = 0;
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

/** The recorded list of dispatched commands, in call order. */
export function getDispatchedCommands(): readonly Command[] {
  return state.dispatched;
}

/** The recorded list of opened project paths, in call order. */
export function getOpenedProjects(): readonly string[] {
  return state.openedProjects;
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
  for (const l of set) {
    (l as Listener<T>).handler({ event: eventName, id, payload });
  }
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
      const reply =
        state.dispatchReplies[cmd.command] ?? state.defaultDispatchReply(cmd);
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
