// Top bar (UI_UX_DESIGN.md §5.1, §4).
//
// Zoned: brand + project + branch switcher · undo/redo · search/⌘K · the SINGLE
// node-action toolbar (gated) · default-model selector · settings · daemon status
// dot. The model selector sets the default for NEW nodes only (real per-node
// multi-provider routing is P6). Branch/model use lightweight dropdowns.

import { useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { ActionToolbar } from "./ActionToolbar";
import { useActions } from "./useActions";
import { Button, IconButton } from "../ui/Button";
import { Icon } from "../ui/icons";
import { SettingsPopover } from "./overlays/SettingsPopover";
import { humanizeModel } from "../ui/format";
import type { NodeView } from "../ipc/types";

/**
 * Model choices, as `provider/model` selector keys (P6 multi-provider routing).
 * The selector sets the default an agent run uses; the router resolves the key to
 * a provider (privacy enforced) and prices the turn. Local + CLI run offline.
 */
export const MODELS = [
  "anthropic/claude-opus-4-8",
  "anthropic/claude-sonnet-4-6",
  "openai/gpt-4o",
  "local/llama3.1",
  "cli/copilot-cli",
] as const;

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
  const selectNode = useUiStore((s) => s.selectNode);
  const setPaletteOpen = useUiStore((s) => s.setPaletteOpen);
  const settingsOpen = useUiStore((s) => s.settingsOpen);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);
  const connection = useUiStore((s) => s.connection);
  const { run } = useActions();

  const [branchMenu, setBranchMenu] = useState(false);
  const [modelMenu, setModelMenu] = useState(false);

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;

  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const branches = view.refs.filter((r) => r.kind === "Branch");
  const currentBranch =
    branches.find((b) => b.target === headTarget)?.name ?? "main";

  return (
    <header className="spork-topbar" aria-label="Top bar">
      {/* zone A: identity + branch */}
      <span className="spork-brand">
        <span className="spork-brand-mark" aria-hidden="true" />
        spork
      </span>
      <span className="spork-project">spork</span>
      <div style={{ position: "relative" }}>
        <Button
          className="spork-branch-btn"
          size="sm"
          variant="ghost"
          icon="git-branch"
          aria-haspopup="menu"
          aria-expanded={branchMenu}
          onClick={() => setBranchMenu((v) => !v)}
        >
          <span className="spork-branch-name">{currentBranch}</span>
          <Icon name="chevron-down" size={12} />
        </Button>
        {branchMenu && (
          <>
            <Backdrop onClose={() => setBranchMenu(false)} />
            <div
              className="spork-ctxmenu"
              role="menu"
              style={{ position: "absolute", top: "100%", left: 0, marginTop: 4, zIndex: 31 }}
            >
              {branches.length === 0 && (
                <span className="spork-ctx-item" aria-disabled>
                  No branches
                </span>
              )}
              {branches.map((b) => (
                <button
                  key={b.name}
                  className="spork-ctx-item"
                  role="menuitem"
                  onClick={() => {
                    selectNode(b.target);
                    setBranchMenu(false);
                  }}
                >
                  <Icon name="git-branch" size={13} />
                  {b.name}
                  {b.target === headTarget && (
                    <span className="spork-ctx-shortcut">HEAD</span>
                  )}
                </button>
              ))}
            </div>
          </>
        )}
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
          {humanizeModel(defaultModel)}
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
              {MODELS.map((m) => (
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
                  {m}
                </button>
              ))}
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
