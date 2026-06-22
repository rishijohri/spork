// Real local-model discovery (no-stub honesty — REALIGNMENT_PLAN §2).
//
// The model selector must only ever offer models the daemon can actually reach.
// This probes the configured (or default) local OpenAI-compatible endpoint for
// the models *really* installed on it and updates the store — replacing the old
// hardcoded `llama3.1` guess. A CLI agent surfaces its single `cli/<command>`
// key; a local endpoint surfaces only what the probe returns.

import { listLocalModels } from "../ipc/client";
import { useUiStore } from "../state/store";

/** The conventional local endpoint probed when none is explicitly configured. */
export const DEFAULT_LOCAL_ENDPOINT = "http://127.0.0.1:11434/v1/chat/completions";

/**
 * Probe the configured (or default) local endpoint and update the store's real
 * model list + the default-model selection. Honest by construction: a failure or
 * an empty listing leaves the selector empty (a "configure a provider" hint),
 * never a fabricated model.
 */
export async function refreshLocalModels(): Promise<void> {
  const provider = useUiStore.getState().agentProvider;

  // A CLI agent has no local listing to probe — its single key is `cli/<command>`.
  if (provider?.kind === "cli") {
    useUiStore.getState().setLocalModels([]);
    const key = `cli/${provider.command?.trim() || "agent"}`;
    if (useUiStore.getState().defaultModel !== key) {
      useUiStore.getState().setDefaultModel(key);
    }
    return;
  }

  const endpoint = provider?.endpoint?.trim() || DEFAULT_LOCAL_ENDPOINT;
  let models: string[] = [];
  try {
    models = await listLocalModels(endpoint);
  } catch {
    models = [];
  }
  useUiStore.getState().setLocalModels(models);

  // Keep the default model valid: pick the first real model if none is selected
  // (or the current one vanished); clear it if the local list went empty.
  const keys = models.map((m) => `local/${m}`);
  const cur = useUiStore.getState().defaultModel;
  if (keys.length > 0 && !keys.includes(cur)) {
    useUiStore.getState().setDefaultModel(keys[0]!);
  } else if (keys.length === 0 && (cur === "" || cur.startsWith("local/"))) {
    useUiStore.getState().setDefaultModel("");
  }
}
