// Top bar (UI_UX_DESIGN.md §5.1, §4; REALIGNMENT_PLAN §5a).
//
// Zoned: brand + project + an informational **line breadcrumb** (NOT a branch
// switcher — lines are emergent, never managed) · a canvas ⇄ chat view toggle ·
// undo/redo · search/⌘K · the SINGLE node-action toolbar (gated) · default-model
// selector · settings · daemon status dot. The model selector sets the default
// for NEW nodes only.

import { useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { ActionToolbar } from "./ActionToolbar";
import { useActions } from "./useActions";
import { Button, IconButton } from "../ui/Button";
import { Icon } from "../ui/icons";
import { SettingsPopover } from "./overlays/SettingsPopover";
import { humanizeModel } from "../ui/format";
import { lineOfNode } from "../state/lines";
import type { NodeView } from "../ipc/types";
import type { AgentProvider } from "../state/store";

/**
 * The model selector keys that are actually **backed** by the configured
 * provider (no stub — REALIGNMENT_PLAN.md §2; the renderer must never advertise a
 * model the daemon cannot reach). A CLI agent surfaces its single `cli/<command>`
 * key; a local endpoint surfaces the models actually **detected** on it
 * (`localModels`, probed from the server — empty until detected, so there is no
 * fake default like the old hardcoded `llama3.1`). Cloud BYOK models (Anthropic /
 * OpenAI / Google) are offered only once a key + the TLS transport exist (R6), so
 * they are not returned yet. An empty result means "no model configured" — the UI
 * shows a configure-a-provider hint, never a fabricated model.
 */
export function availableModels(
  provider: AgentProvider | null,
  localModels: readonly string[] = [],
): string[] {
  if (provider?.kind === "cli") {
    return [`cli/${provider.command?.trim() || "agent"}`];
  }
  // A local endpoint (or the daemon default, which is local): only the real,
  // detected models — never a hardcoded guess.
  return localModels.map((m) => `local/${m}`);
}

const CONN_LABEL: Record<string, string> = {
  connected: "Daemon connected",
  reconnecting: "Reconnecting…",
  disconnected: "Daemon disconnected",
  "no-project": "No project open",
  mock: "Browser mock mode",
};

/** A tiny click-away backdrop for the inline dropdowns. */
function Backdrop({ onClose }: { onClose: () => void }): JSX.Element {
  return (
    <div
      onClick={onClose}
      style={{ position: "fixed", inset: 0, zIndex: 30 }}
      aria-hidden="true"
    />
  );
}

/** The top-region bar. */
export function TopBar(): JSX.Element {
  const view = useUiStore((s) => s.view);
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const defaultModel = useUiStore((s) => s.defaultModel);
  const setDefaultModel = useUiStore((s) => s.setDefaultModel);
  const agentProvider = useUiStore((s) => s.agentProvider);
  const localModels = useUiStore((s) => s.localModels);
  const setPaletteOpen = useUiStore((s) => s.setPaletteOpen);
  const settingsOpen = useUiStore((s) => s.settingsOpen);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);
  const connection = useUiStore((s) => s.connection);
  const centerView = useUiStore((s) => s.centerView);
  const setCenterView = useUiStore((s) => s.setCenterView);
  const { run } = useActions();

  const [modelMenu, setModelMenu] = useState(false);

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;

  // The current line is the selected node's line, else the HEAD's line, else main.
  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const currentLine =
    (selectedId && lineOfNode(view, selectedId)) ||
    (headTarget && lineOfNode(view, headTarget)) ||
    null;
  const lineLabel = currentLine?.label ?? "main line";
  const models = availableModels(agentProvider, localModels);

  return (
    <header className="spork-topbar" aria-label="Top bar">
      {/* zone A: identity + the (informational) line breadcrumb */}
      <span className="spork-brand">
        <span className="spork-brand-mark" aria-hidden="true" />
        spork
      </span>
      <span className="spork-project">spork</span>
      <span
        className="spork-line-crumb"
        title="The current line — an emergent line of work, not a git branch you switch"
      >
        <Icon name="git-commit" size={13} />
        <span className="spork-line-name">{lineLabel}</span>
      </span>

      <div className="spork-topbar-sep" />

      {/* zone A2: canvas ⇄ chat — two views of the same node substrate */}
      <div className="spork-seg spork-viewtoggle" role="group" aria-label="Center view">
        <button
          aria-pressed={centerView === "canvas"}
          onClick={() => setCenterView("canvas")}
          title="Timeline canvas"
        >
          <Icon name="git-branch" size={13} /> Canvas
        </button>
        <button
          aria-pressed={centerView === "chat"}
          onClick={() => setCenterView("chat")}
          title="Hero chat"
        >
          <Icon name="messages-square" size={13} /> Chat
        </button>
      </div>

      <div className="spork-topbar-sep" />

      {/* zone B: history */}
      <IconButton
        icon="rotate-ccw"
        label="Undo (⌘Z)"
        onClick={() => void run({ command: "OP_UNDO", opId: null }, { label: "Undo" })}
      />
      <IconButton
        icon="rotate-cw"
        label="Redo (⇧⌘Z)"
        onClick={() => void run({ command: "OP_REDO", opId: null }, { label: "Redo" })}
      />

      <div className="spork-topbar-sep" />

      {/* zone C: search / palette */}
      <button
        className="spork-search"
        onClick={() => setPaletteOpen(true)}
        aria-label="Search nodes and actions"
      >
        <Icon name="search" size={14} />
        <span>Search…</span>
        <span className="spork-kbd">⌘K</span>
      </button>

      <div className="spork-topbar-spacer" />

      {/* zone D: node actions (single instance) */}
      <ActionToolbar node={node} />

      <div className="spork-topbar-sep" />

      {/* zone E: model + settings + status */}
      <div style={{ position: "relative" }}>
        <Button
          className="spork-model-btn"
          size="sm"
          variant="ghost"
          title="Default model for new nodes"
          aria-haspopup="menu"
          aria-expanded={modelMenu}
          onClick={() => setModelMenu((v) => !v)}
        >
          <span className="spork-model-dot" />
          {defaultModel ? humanizeModel(defaultModel) : "No model"}
          <Icon name="chevron-down" size={12} />
        </Button>
        {modelMenu && (
          <>
            <Backdrop onClose={() => setModelMenu(false)} />
            <div
              className="spork-ctxmenu"
              role="menu"
              style={{ position: "absolute", top: "100%", right: 0, marginTop: 4, zIndex: 31 }}
            >
              <span className="spork-ctx-item spork-faint" aria-disabled style={{ fontSize: 11 }}>
                Default for new nodes
              </span>
              {models.length === 0 ? (
                <button
                  className="spork-ctx-item"
                  role="menuitem"
                  onClick={() => {
                    setModelMenu(false);
                    setSettingsOpen(true);
                  }}
                >
                  <Icon name="settings" size={13} />
                  No models — set up a provider…
                </button>
              ) : (
                models.map((m) => (
                  <button
                    key={m}
                    className="spork-ctx-item"
                    role="menuitemradio"
                    aria-checked={m === defaultModel}
                    onClick={() => {
                      setDefaultModel(m);
                      setModelMenu(false);
                    }}
                  >
                    {m === defaultModel ? <Icon name="check" size={13} /> : <span style={{ width: 13 }} />}
                    {humanizeModel(m)}
                  </button>
                ))
              )}
            </div>
          </>
        )}
      </div>

      <div style={{ position: "relative" }}>
        <IconButton
          icon="settings"
          label="Settings"
          active={settingsOpen}
          onClick={() => setSettingsOpen(!settingsOpen)}
        />
        {settingsOpen && (
          <>
            <Backdrop onClose={() => setSettingsOpen(false)} />
            <div style={{ position: "absolute", top: "100%", right: 0, marginTop: 6, zIndex: 41 }}>
              <SettingsPopover />
            </div>
          </>
        )}
      </div>

      <span className="spork-conn" data-state={connection} title={CONN_LABEL[connection]}>
        <span className="spork-conn-dot" aria-hidden="true" />
      </span>
    </header>
  );
}
