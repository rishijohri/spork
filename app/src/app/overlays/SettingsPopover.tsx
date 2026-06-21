// Settings popover (UI_UX_DESIGN.md §5.13).
//
// The few real v1 preferences (🟢): theme (dark-only in v1), density, a
// panel-layout reset, the default-model default, and — P7.5 MVP (W3) — the agent
// **provider** (a local OpenAI-compatible HTTP endpoint, the default route, or a
// *conforming* CLI agent that speaks Spork's JSONL protocol). The provider is
// configured here once and persisted by the daemon under
// <project>/.spork/agent_config.json; the endpoint/CLI command is config, not a
// secret (DESIGN.md §15.1). The generic CLI-as-model route is deprecated
// (REALIGNMENT_PLAN.md §2): a real coding CLI (claude/copilot/cursor) should
// drive Spork over the Orchestration MCP, not be driven as a model.
// Engine-health / Extensions are forward-map (P8).

import { useState, type JSX } from "react";
import { useUiStore, type AgentProvider } from "../../state/store";
import { MODELS, availableModels } from "../TopBar";
import { humanizeModel } from "../../ui/format";
import {
  setAgentConfig,
  dispatch,
  type AgentProviderConfig,
} from "../../ipc/client";

/** The default local endpoint the provider form prefills (Ollama/LM-Studio). */
const DEFAULT_LOCAL_ENDPOINT = "http://127.0.0.1:11434/v1/chat/completions";

/** The agent-provider configuration section (P7.5 MVP, W3). */
function ProviderSection(): JSX.Element {
  const agentProvider = useUiStore((s) => s.agentProvider);
  const setAgentProvider = useUiStore((s) => s.setAgentProvider);
  const setDefaultModel = useUiStore((s) => s.setDefaultModel);
  const logActivity = useUiStore((s) => s.logActivity);
  const view = useUiStore((s) => s.view);

  const [kind, setKind] = useState<"local" | "cli">(agentProvider?.kind ?? "local");
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
      const backed = availableModels(provider);
      if (backed[0]) setDefaultModel(backed[0]);
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
      const model = availableModels(provider)[0] ?? "";
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
      <span className="spork-eyebrow">Agent provider</span>
      <div className="spork-row-between">
        <span>Provider</span>
        <div className="spork-seg" role="group" aria-label="Agent provider">
          <button aria-pressed={kind === "local"} onClick={() => setKind("local")}>
            Local endpoint
          </button>
          <button aria-pressed={kind === "cli"} onClick={() => setKind("cli")}>
            CLI agent
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
          Save provider
        </button>
        <button className="btn" disabled={busy} onClick={() => void test()}>
          Test connection
        </button>
      </div>
      <span className="spork-muted" style={{ fontSize: 11 }}>
        Saved to <code>.spork/agent_config.json</code> in the project. The endpoint
        / command is config, never a secret.
      </span>
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
          <select
            value={defaultModel}
            onChange={(e) => setDefaultModel(e.target.value)}
            aria-label="Default model for new nodes"
          >
            {MODELS.map((m) => (
              <option key={m} value={m}>
                {humanizeModel(m)}
              </option>
            ))}
          </select>
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
