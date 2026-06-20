// The shared action toolbar (DESIGN.md §14.2, §14.5).
//
// Renders the frozen `TOOLBAR_ACTIONS` set as buttons, each enabled/disabled per
// the current selection via `enabledWhen(node)`, and each WIRED to dispatch its
// `Command` via `runToolbarAction` with optimistic UI. Both the top bar and the
// node-details panel render this component, so the gating and the dispatch path
// are defined in exactly one place (DESIGN.md §14.5 — schema-driven gating).
//
// Optimistic UI (DESIGN.md §14.4): on click the action dispatches and the
// returned `opId` is registered as a pending op (`beginOptimistic`); the button
// shows the in-flight state until the op-log stream's reconciling event lands and
// the store clears it. The resulting graph state arrives ONLY over the op-log
// stream — never the dispatch return value.

import { useCallback, useState } from "react";
import { useUiStore } from "../state/store";
import { TOOLBAR_ACTIONS, runToolbarAction, type ToolbarAction } from "./toolbar";
import type { NodeView } from "../ipc/types";

export interface ActionToolbarProps {
  /** The current selection the actions gate + dispatch against (null = none). */
  node: NodeView | null;
  /** An aria-label distinguishing the top-bar vs. the node-panel instance. */
  ariaLabel: string;
  /**
   * Optional per-action side effect run after the action settles (e.g. the
   * node-details panel switches its tab to "diff" when View is clicked). Local
   * UI only — the daemon path is unchanged.
   */
  onActionDone?: (action: ToolbarAction) => void;
}

/** The wired action toolbar shared by the top bar and the node-details panel. */
export function ActionToolbar({
  node,
  ariaLabel,
  onActionDone,
}: ActionToolbarProps): JSX.Element {
  const defaultModel = useUiStore((s) => s.defaultModel);
  const beginOptimistic = useUiStore((s) => s.beginOptimistic);
  const logActivity = useUiStore((s) => s.logActivity);
  // The set of action ids with an in-flight dispatch, so the button reflects the
  // optimistic state until its op-log event reconciles.
  const [inFlight, setInFlight] = useState<ReadonlySet<string>>(new Set());

  const onAction = useCallback(
    async (action: ToolbarAction): Promise<void> => {
      setInFlight((s) => new Set(s).add(action.id));
      try {
        // `runToolbarAction` never throws (it catches + logs errors itself), so
        // the activity log captures every outcome and the UI never crashes.
        await runToolbarAction(
          action,
          node,
          { defaultModel },
          { beginOptimistic, logActivity },
        );
        onActionDone?.(action);
      } finally {
        setInFlight((s) => {
          const next = new Set(s);
          next.delete(action.id);
          return next;
        });
      }
    },
    [node, defaultModel, beginOptimistic, logActivity, onActionDone],
  );

  return (
    <div className="spork-toolbar" role="toolbar" aria-label={ariaLabel}>
      {TOOLBAR_ACTIONS.map((a) => {
        const enabled = a.enabledWhen(node);
        const busy = inFlight.has(a.id);
        return (
          <button
            key={a.id}
            disabled={!enabled || busy}
            data-action={a.id}
            data-busy={busy ? "true" : undefined}
            aria-disabled={!enabled || busy}
            onClick={() => void onAction(a)}
          >
            {a.label}
          </button>
        );
      })}
    </div>
  );
}
