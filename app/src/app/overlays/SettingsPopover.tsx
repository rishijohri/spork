// Settings popover (UI_UX_DESIGN.md §5.13; REALIGNMENT_PLAN §4, §5d).
//
// The two connectivity arrows are now configured in SEPARATE sections, since they
// have different requirements (the user's ask):
//   • Driving model  — Spork drives a model for its own nodes. Local server
//     (Ollama/LM Studio/vLLM, with REAL model detection — no fake default) or a
//     conforming CLI; cloud BYOK (Anthropic/OpenAI/Google) is shown honestly as
//     landing next (it needs the TLS transport, R6).
//   • Orchestrator   — your existing agent (Claude Code/Copilot/Cursor) drives
//     Spork via the write-capable Orchestration MCP (R5; forward-mapped).
// The local endpoint / CLI command is config, not a secret (DESIGN.md §15.1); a
// cloud key is resolved only inside the daemon (vault, `vaultRef`). Other v1
// prefs (🟢): theme, density, panel reset, default model, editor. Engine-health /
// Extensions are forward-map (P8).

import { useState, type JSX } from "react";
import { useUiStore, type AgentProvider } from "../../state/store";
import { availableModels } from "../TopBar";
import { refreshLocalModels, DEFAULT_LOCAL_ENDPOINT } from "../models";
import { humanizeModel } from "../../ui/format";
import {
  setAgentConfig,
  dispatch,
  type AgentProviderConfig,
} from "../../ipc/client";

/** The agent-provider configuration section (P7.5 MVP, W3). */
function ProviderSection(): JSX.Element {
  const agentProvider = useUiStore((s) => s.agentProvider);
  const setAgentProvider = useUiStore((s) => s.setAgentProvider);
  const localModels = useUiStore((s) => s.localModels);
  const logActivity = useUiStore((s) => s.logActivity);
  const view = useUiStore((s) => s.view);

  const [kind, setKind] = useState<"local" | "cli">(agentProvider?.kind ?? "local");
  const [cloudProvider, setCloudProvider] = useState<"anthropic" | "openai" | "google">("anthropic");
  const [endpoint, setEndpoint] = useState(
    agentProvider?.endpoint ?? DEFAULT_LOCAL_ENDPOINT,
  );
  const [command, setCommand] = useState(agentProvider?.command ?? "");
  const [args, setArgs] = useState((agentProvider?.args ?? []).join(" "));
  const [busy, setBusy] = useState(false);

  function buildConfig(): AgentProviderConfig {
    if (kind === "cli") {
      const a = args.trim();
      return {
        kind: "cli",
        command: command.trim(),
        args: a ? a.split(/\s+/) : [],
      };
    }
    return { kind: "local", endpoint: endpoint.trim() };
  }

  /** Persist the provider (and point the default model at a backed key). */
  async function save(): Promise<AgentProvider | null> {
    setBusy(true);
    try {
      const cfg = buildConfig();
      await setAgentConfig(cfg);
      const provider: AgentProvider = { ...cfg };
      setAgentProvider(provider);
      // Probe the endpoint for its REAL installed models + set an honest default
      // (no hardcoded guess) — `refreshLocalModels` reads the just-set provider.
      await refreshLocalModels();
      logActivity(
        "success",
        kind === "cli"
          ? `Provider set: CLI ${cfg.command}`
          : `Provider set: ${cfg.endpoint}`,
      );
      return provider;
    } catch (err) {
      logActivity(
        "error",
        `Provider config failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      return null;
    } finally {
      setBusy(false);
    }
  }

  /** Apply the provider, then send a trivial read-only ask to verify it. */
  async function test(): Promise<void> {
    const provider = await save();
    if (!provider) return;
    const head =
      view.refs.find((r) => r.name === "HEAD")?.target ?? view.nodes[0]?.id;
    if (!head) {
      logActivity("error", "Open a project first to test the provider.");
      return;
    }
    setBusy(true);
    try {
      const model =
        useUiStore.getState().defaultModel ||
        availableModels(provider, useUiStore.getState().localModels)[0] ||
        "";
      const result = await dispatch({
        command: "NODE_AGENT_RUN",
        targetNodeId: head,
        prompt: "Reply with OK.",
        modelKey: model,
        privacy: "any",
        intent: "ask",
      });
      const ok =
        result.result === "MUTATION"
          ? (result.ids["provider"] as string | undefined)
          : undefined;
      logActivity("success", `Provider responded (${ok ?? "ok"})`);
    } catch (err) {
      logActivity(
        "error",
        `Test failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="spork-popover-group">
      <span className="spork-eyebrow">Driving model</span>
      <span className="spork-muted" style={{ fontSize: 11 }}>
        Spork drives a model for its own in-app nodes (the chat + agentic runs).
        This is separate from connecting your own agent as an orchestrator (below).
      </span>
      <div className="spork-row-between">
        <span>Source</span>
        <div className="spork-seg" role="group" aria-label="Driving model source">
          <button aria-pressed={kind === "local"} onClick={() => setKind("local")}>
            Local server
          </button>
          <button aria-pressed={kind === "cli"} onClick={() => setKind("cli")}>
            Conforming CLI
          </button>
        </div>
      </div>

      {kind === "local" ? (
        <label className="spork-field">
          <span className="spork-muted" style={{ fontSize: 11 }}>
            OpenAI-compatible endpoint URL
          </span>
          <input
            type="text"
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder={DEFAULT_LOCAL_ENDPOINT}
            aria-label="Local endpoint URL"
          />
        </label>
      ) : (
        <>
          <label className="spork-field">
            <span className="spork-muted" style={{ fontSize: 11 }}>
              CLI command
            </span>
            <input
              type="text"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="a conforming JSONL agent (not claude/copilot)"
              aria-label="CLI agent command"
            />
          </label>
          <span className="spork-muted" style={{ fontSize: 11 }} role="note">
            A real coding CLI (claude/copilot/cursor) owns its own tool loop and
            can&rsquo;t be driven as a model. To let one drive Spork, connect it
            over the Orchestration MCP (coming soon). For Spork to drive a model,
            use a local endpoint above.
          </span>
          <label className="spork-field">
            <span className="spork-muted" style={{ fontSize: 11 }}>
              Arguments (space-separated)
            </span>
            <input
              type="text"
              value={args}
              onChange={(e) => setArgs(e.target.value)}
              placeholder="--json"
              aria-label="CLI agent arguments"
            />
          </label>
        </>
      )}

      <div style={{ display: "flex", gap: 8 }}>
        <button className="btn btn--primary" disabled={busy} onClick={() => void save()}>
          {busy ? "Saving…" : kind === "local" ? "Save & detect models" : "Save provider"}
        </button>
        <button className="btn" disabled={busy} onClick={() => void test()}>
          Test connection
        </button>
      </div>

      {kind === "local" && localModels.length > 0 && (
        <div className="spork-model-chips" role="status" aria-label="Detected models">
          <span className="spork-muted" style={{ fontSize: 11 }}>
            Detected on this server:
          </span>
          {localModels.map((m) => (
            <span key={m} className="spork-chip" title={m}>
              {m}
            </span>
          ))}
        </div>
      )}

      <span className="spork-muted" style={{ fontSize: 11 }}>
        Saved to <code>.spork/agent_config.json</code> in the project. The endpoint
        / command is config, never a secret.
      </span>

      {/* Cloud (BYOK) — honest forward-map: keys go in the daemon vault and drive
          over TLS, which is the transport landing next (R6). Shown, not faked. */}
      <div className="spork-byok">
        <div className="spork-row-between">
          <span style={{ fontWeight: 600, fontSize: 12 }}>Cloud (BYOK)</span>
          <span className="spork-pill-soon">landing next</span>
        </div>
        <div className="spork-seg" role="group" aria-label="Cloud provider">
          {(["anthropic", "openai", "google"] as const).map((p) => (
            <button key={p} aria-pressed={cloudProvider === p} onClick={() => setCloudProvider(p)}>
              {CLOUD_LABEL[p]}
            </button>
          ))}
        </div>
        <label className="spork-field">
          <span className="spork-muted" style={{ fontSize: 11 }}>
            {CLOUD_LABEL[cloudProvider]} API key
          </span>
          <input
            type="password"
            placeholder="Secure key vault + TLS — landing next"
            aria-label="Cloud API key"
            disabled
          />
        </label>
        <span className="spork-muted" style={{ fontSize: 11 }}>
          Your key is resolved inside the daemon (vault, <code>vaultRef</code>) and
          sent over TLS — the renderer never holds it. The secure transport is in
          progress; until then, use a local server above.
        </span>
      </div>
    </div>
  );
}

/** Friendly cloud-provider labels (BYOK). */
const CLOUD_LABEL: Record<"anthropic" | "openai" | "google", string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  google: "Google",
};

/** The Orchestrator-connection section (REALIGNMENT_PLAN §4, R5) — your existing
 *  agent drives Spork over a write-capable MCP. Honest forward-map until R5. */
function OrchestratorSection(): JSX.Element {
  return (
    <div className="spork-popover-group">
      <span className="spork-eyebrow">Orchestrator connection</span>
      <span className="spork-muted" style={{ fontSize: 11 }}>
        Your existing agent (Claude Code / Copilot / Cursor) drives Spork&rsquo;s
        timeline via a write-capable <strong>Orchestration MCP</strong> — the
        &ldquo;Spork Node skill.&rdquo; This is the opposite arrow from a driving
        model: your agent creates and runs nodes here.
      </span>
      <div className="spork-row-between">
        <span style={{ fontWeight: 600, fontSize: 12 }}>Orchestration MCP</span>
        <span className="spork-pill-soon">coming (R5)</span>
      </div>
      <code className="spork-code-line">
        claude mcp add spork -- spork-mcp-orchestrate --project &lt;path&gt;
      </code>
      <label className="spork-checkbox" title="Enabled with the Orchestration MCP (R5)">
        <input type="checkbox" disabled /> Allow an external orchestrator to
        create / run nodes
      </label>
    </div>
  );
}

/** The settings popover body. */
export function SettingsPopover(): JSX.Element {
  const density = useUiStore((s) => s.density);
  const setDensity = useUiStore((s) => s.setDensity);
  const defaultModel = useUiStore((s) => s.defaultModel);
  const setDefaultModel = useUiStore((s) => s.setDefaultModel);
  const editorPref = useUiStore((s) => s.editorPref);
  const setEditorPref = useUiStore((s) => s.setEditorPref);
  const agentProvider = useUiStore((s) => s.agentProvider);
  const localModels = useUiStore((s) => s.localModels);
  const models = availableModels(agentProvider, localModels);

  function resetPanels(): void {
    const st = useUiStore.getState();
    if (st.navCollapsed) st.toggleNav();
    if (st.detailsCollapsed) st.toggleDetails();
    st.setRailCollapsed(false);
  }

  return (
    <div className="spork-popover" role="dialog" aria-label="Settings">
      <div className="spork-popover-group">
        <span className="spork-eyebrow">Appearance</span>
        <div className="spork-row-between">
          <span>Theme</span>
          <div className="spork-seg" role="group" aria-label="Theme">
            <button aria-pressed={true}>Dark</button>
            <button aria-pressed={false} disabled title="Light theme arrives later">
              Light
            </button>
          </div>
        </div>
        <div className="spork-row-between">
          <span>Density</span>
          <div className="spork-seg" role="group" aria-label="Density">
            <button
              aria-pressed={density === "comfortable"}
              onClick={() => setDensity("comfortable")}
            >
              Comfortable
            </button>
            <button
              aria-pressed={density === "compact"}
              onClick={() => setDensity("compact")}
            >
              Compact
            </button>
          </div>
        </div>
      </div>

      <ProviderSection />

      <OrchestratorSection />

      <div className="spork-popover-group">
        <span className="spork-eyebrow">Layout</span>
        <button className="btn" onClick={resetPanels}>
          Reset panels
        </button>
      </div>

      <div className="spork-popover-group">
        <span className="spork-eyebrow">Models</span>
        <div className="spork-row-between">
          <span>Default for new nodes</span>
          {models.length === 0 ? (
            <span className="spork-muted" style={{ fontSize: 12 }}>
              Configure a provider above
            </span>
          ) : (
            <select
              value={defaultModel}
              onChange={(e) => setDefaultModel(e.target.value)}
              aria-label="Default model for new nodes"
            >
              {models.map((m) => (
                <option key={m} value={m}>
                  {humanizeModel(m)}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      <div className="spork-popover-group">
        <span className="spork-eyebrow">Editor</span>
        <label className="spork-field">
          <span className="spork-muted" style={{ fontSize: 11 }}>
            "Open codebase" hands off to this editor (Spork is not a code editor).
          </span>
          <input
            type="text"
            value={editorPref}
            onChange={(e) => setEditorPref(e.target.value)}
            placeholder="auto-detect (code / cursor / subl / idea)"
            aria-label="Editor launcher"
          />
        </label>
      </div>

      <div className="spork-fwd-note">
        Engine health &amp; Extensions settings arrive in P8.
      </div>
    </div>
  );
}
