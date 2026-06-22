// Onboarding helpers (P7.5 MVP, W1/W2) — open a project, capture its first node,
// and remember it so a returning user is auto-reopened instead of dropped back to
// the picker every launch.
//
// `openAndImport` is the single open→import→refetch path shared by the empty-state
// picker and the Shell mount-time auto-open. The daemon's `PROJECT_IMPORT` is
// idempotent (a reopen returns the existing root, no duplicate, no error), so this
// is safe to call on every open.

import type { QueryClient } from "@tanstack/react-query";
import { openProject, dispatch } from "../ipc/client";
import { GRAPH_VIEW_KEY } from "../state/queries";

/** The localStorage key holding the last successfully opened project path. */
const LAST_PROJECT_KEY = "spork.lastProject";

/** Remember the last opened project path so it can be auto-reopened next launch. */
export function rememberProject(path: string): void {
  try {
    localStorage.setItem(LAST_PROJECT_KEY, path);
  } catch {
    /* storage unavailable — non-fatal, just no auto-reopen */
  }
}

/** The last opened project path, or null if none / storage unavailable. */
export function lastProject(): string | null {
  try {
    return localStorage.getItem(LAST_PROJECT_KEY);
  } catch {
    return null;
  }
}

/** Forget the remembered project (e.g. after it fails to reopen). */
export function forgetProject(): void {
  try {
    localStorage.removeItem(LAST_PROJECT_KEY);
  } catch {
    /* ignore */
  }
}

/**
 * Open a daemon over `path`, capture/refresh its first node, and refetch the
 * view-model so the canvas renders. Returns the root node id (or undefined). The
 * import is idempotent, so this both onboards a fresh project and reopens an
 * existing one.
 */
export async function openAndImport(
  path: string,
  qc: QueryClient,
): Promise<string | undefined> {
  await openProject(path);
  const result = await dispatch({
    command: "PROJECT_IMPORT",
    branchId: "main",
    origin: "import",
  });
  // Always refetch — whether the import minted a new root or returned an existing
  // one (reopen) — so the canvas leaves the empty state and renders the nodes.
  await qc.invalidateQueries({ queryKey: GRAPH_VIEW_KEY });
  return result.result === "MUTATION" ? (result.ids["nodeId"] as string) : undefined;
}
