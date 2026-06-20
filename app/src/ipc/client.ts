// The typed IPC client (DESIGN.md §14.1, §14.4, A.1).
//
// A thin, typed wrapper around the Tauri command surface — the ONLY path the
// renderer uses to reach the daemon. It binds exactly to the three backend
// commands (`open_project`, `dispatch`, `graph_view`) and the two forwarded
// event channels (`oplog-event`, `ephemeral`). The renderer never touches the
// filesystem or providers directly (DESIGN.md §15.1).
//
// Runtime selection (two modes, one surface):
//   - TAURI present  → route to the real `@tauri-apps/api` invoke/listen, the
//     production daemon path.
//   - TAURI absent   → route to the in-memory mock (src/ipc/mock.ts). This is
//     what makes a PLAIN BROWSER (the Vite dev server at localhost:1420, and
//     Vitest) work for UX testing: a plain browser has no Tauri runtime, so the
//     real `listen()` would throw "Cannot read properties of undefined
//     (reading 'transformCallback')" on mount and the whole app would be dead.
//     In mock mode everything (dispatch / graphView / openProject AND the event
//     subscriptions) runs against the seeded demo DAG, so the canvas renders and
//     the optimistic-UI/reducer path is exercised with no daemon and no throw.
//
// Tauri v2 exposes its runtime as `window.__TAURI_INTERNALS__`; we detect that
// once at module load.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type {
  Command,
  CommandResult,
  EphemeralFrame,
  GraphView,
  OpLogEvent,
  TauriEvent,
} from "./types";
import { OPLOG_EVENT, EPHEMERAL_EVENT } from "./types";
import { mockInvoke, mockListen, seedDemoGraph } from "./mock";

/**
 * Whether a Tauri v2 runtime is present. Tauri v2 injects
 * `window.__TAURI_INTERNALS__` into the webview; a plain browser (the Vite dev
 * server, Vitest's jsdom) has no such global, so this is `false` there and the
 * client routes every call to the in-memory mock instead of `invoke`/`listen`.
 */
export function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !==
      "undefined"
  );
}

// Cache the decision once: the runtime does not appear/disappear at run time.
const TAURI = isTauri();

// In a plain browser (no Tauri), seed the mock with a small demo DAG so the
// canvas renders something at localhost:1420, and leave a console hint. This
// runs once at module load. Under Vitest `window.__TAURI_INTERNALS__` is also
// absent, but the test setup resets the mock per-test, so seeding here is a
// harmless default the tests overwrite as needed.
if (!TAURI && typeof window !== "undefined") {
  seedDemoGraph();
  // eslint-disable-next-line no-console
  console.info("Spork: running in browser mock mode (no Tauri runtime)");
}

/** The active `invoke` implementation for this runtime. */
const invoke = TAURI ? tauriInvoke : mockInvoke;
/** The active `listen` implementation for this runtime. */
const listen = TAURI ? tauriListen : mockListen;

/** Build/open a daemon over a project repo. Mirrors the `open_project` command. */
export async function openProject(path: string): Promise<void> {
  await invoke<void>("open_project", { path });
}

/**
 * Dispatch one typed `Command` and return the parsed `CommandResult`.
 *
 * This is the single mutation/read path (DESIGN.md A.1): a mutation returns a
 * `MUTATION` result carrying the correlation `opId`; reads (and the action-shaped
 * git commands) return their data inline. The resulting graph state of a mutation
 * arrives over the op-log event stream, never on this return value.
 */
export async function dispatch(command: Command): Promise<CommandResult> {
  return invoke<CommandResult>("dispatch", { command });
}

/** Fetch the denormalized view-model snapshot. Mirrors the `graph_view` command. */
export async function graphView(): Promise<GraphView> {
  return invoke<GraphView>("graph_view");
}

/** A handle that stops an event subscription when called. */
export type Unlisten = () => void;

/**
 * Subscribe to the ordered durable op-log event stream.
 *
 * Every mutation's resulting state arrives here in `seq` order (DESIGN.md
 * §14.4). The renderer folds these into the view-model via the pure reducer. In
 * browser mock mode this subscribes to the mock's event bus (which the mock's
 * dispatch replies drive), so it never throws on a missing Tauri runtime.
 */
export async function listenOpLog(
  handler: (event: OpLogEvent) => void,
): Promise<Unlisten> {
  return listen<OpLogEvent>(OPLOG_EVENT, (e: TauriEvent<OpLogEvent>) =>
    handler(e.payload),
  );
}

/**
 * Subscribe to the node-keyed ephemeral side-channel (chat tokens / run stdout).
 *
 * Frames are unordered across nodes and safe to drop under backpressure; they
 * never stall the ordered op-log (DESIGN.md §5.5, §14.4).
 */
export async function listenEphemeral(
  handler: (frame: EphemeralFrame) => void,
): Promise<Unlisten> {
  return listen<EphemeralFrame>(EPHEMERAL_EVENT, (e: TauriEvent<EphemeralFrame>) =>
    handler(e.payload),
  );
}
