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
 * Open the OS **folder picker** and return the chosen directory, or `null` if
 * cancelled (P7.5 MVP, W1). Uses the Tauri dialog plugin in the desktop app; in a
 * plain browser / Vitest (no Tauri runtime) it returns `null` so the typed-path
 * fallback is used instead of throwing. The plugin is imported lazily so the
 * browser bundle never evaluates it.
 */
export async function pickProjectDir(): Promise<string | null> {
  if (!TAURI) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({
    directory: true,
    multiple: false,
    title: "Open a project folder",
  });
  return typeof selected === "string" ? selected : null;
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

/**
 * The provider config the Settings form sends to `set_agent_config` (P7.5 MVP,
 * W3). A friendly shape the Rust side maps to the daemon's `AgentConfig`; the
 * endpoint/CLI command is config, not a secret (the renderer holds zero secrets,
 * DESIGN.md §15.1).
 */
export interface AgentProviderConfig {
  /** `"local"` (HTTP endpoint) or `"cli"` (subprocess agent). */
  kind: "local" | "cli";
  /** The local OpenAI-compatible endpoint URL (when `kind === "local"`). */
  endpoint?: string;
  /** The CLI agent program — a *conforming* JSONL agent (when `kind === "cli"`). */
  command?: string;
  /** The CLI agent args (when `kind === "cli"`). */
  args?: string[];
}

/**
 * Configure (and persist) which provider agent runs reach — a local HTTP
 * endpoint or a CLI agent (P7.5 MVP, W3). Mirrors the `set_agent_config` command;
 * the choice is persisted under `<project>/.spork/agent_config.json` and survives
 * a restart.
 */
export async function setAgentConfig(config: AgentProviderConfig): Promise<void> {
  await invoke<void>("set_agent_config", { config });
}

/**
 * Hand a path off to the user's real editor (REALIGNMENT_PLAN §5d) — Spork is not
 * a code editor. `editor` is an optional explicit launcher (`code`/`cursor`/…);
 * absent, the Rust side probes for one and falls back to the OS opener. A no-op
 * in browser mock mode (no Tauri runtime to spawn a process).
 */
export async function openInEditor(path: string, editor?: string): Promise<void> {
  if (!TAURI) return;
  await invoke<void>("open_in_editor", { path, editor: editor ?? null });
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
