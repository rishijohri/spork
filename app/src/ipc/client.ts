// The typed IPC client (DESIGN.md §14.1, §14.4, A.1).
//
// A thin, typed wrapper around the Tauri command surface — the ONLY path the
// renderer uses to reach the daemon. It binds exactly to the three backend
// commands (`open_project`, `dispatch`, `graph_view`) and the two forwarded
// event channels (`oplog-event`, `ephemeral`). The renderer never touches the
// filesystem or providers directly (DESIGN.md §15.1).
//
// Under Vitest, `invoke`/`listen` are mocked (src/test/setup.ts) so this module
// exercises the real serialization/correlation logic against fixtures with no
// daemon and no display.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Command,
  CommandResult,
  EphemeralFrame,
  GraphView,
  OpLogEvent,
  TauriEvent,
} from "./types";
import { OPLOG_EVENT, EPHEMERAL_EVENT } from "./types";

/** Build/open a daemon over a project repo. Mirrors the `open_project` command. */
export async function openProject(path: string): Promise<void> {
  await invoke<void>("open_project", { path });
}

/**
 * Dispatch one typed `Command` and return the parsed `CommandResult`.
 *
 * This is the single mutation/read path (DESIGN.md A.1): a mutation returns a
 * `MUTATION` result carrying the correlation `opId`; reads return their data
 * inline. The resulting graph state of a mutation arrives over the op-log event
 * stream, never on this return value.
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
 * §14.4). The renderer folds these into the view-model via the pure reducer.
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
