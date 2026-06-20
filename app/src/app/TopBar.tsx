// Top bar: model selector + action toolbar (DESIGN.md §14.2).
//
// The quick model selector sets the default model for NEW nodes (held in the UI
// store). The action toolbar mirrors the reference UI; each action is enabled per
// the current selection via `enabledWhen(node)` (DESIGN.md §14.5).

import { useUiStore } from "../state/store";
import { ActionToolbar } from "./ActionToolbar";
import type { NodeView } from "../ipc/types";

/** A small, fixed set of selectable models for the quick selector. */
const MODELS = [
  "claude-3-5-sonnet",
  "claude-3-5-haiku",
  "gpt-4o",
  "o1-preview",
] as const;

/** The top-region bar. */
export function TopBar(): JSX.Element {
  const defaultModel = useUiStore((s) => s.defaultModel);
  const setDefaultModel = useUiStore((s) => s.setDefaultModel);
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const view = useUiStore((s) => s.view);

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;

  return (
    <header className="spork-topbar" aria-label="Top bar">
      <label className="spork-model-selector">
        <span>Model</span>
        <select
          value={defaultModel}
          onChange={(e) => setDefaultModel(e.target.value)}
          aria-label="Default model for new nodes"
        >
          {MODELS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </label>

      <div className="spork-topbar-actions">
        <ActionToolbar node={node} ariaLabel="Actions" />
      </div>
    </header>
  );
}
