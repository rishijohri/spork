// The node-action toolbar (UI_UX_DESIGN.md §5.1, §7.1).
//
// A SINGLE instance, in the top bar, gated by the selected node (§14.2/§14.5) —
// no more duplicate copy in the details panel. Most actions open a modal (so a
// name / remote / confirm is collected); the modal dispatches via the shared
// action runner. Run-check opens a small Validate/Stress/Sanity submenu that
// dispatches directly (with optimistic UI + an activity line).

import { useRef, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { TOOLBAR_ACTIONS, CHECK_KINDS, type ToolbarAction } from "./toolbar";
import { useActions } from "./useActions";
import { Button } from "../ui/Button";
import { Icon } from "../ui/icons";
import type { NodeView } from "../ipc/types";

export interface ActionToolbarProps {
  /** The current selection the actions gate + act on (null = none). */
  node: NodeView | null;
  ariaLabel?: string;
}

/** The single node-action toolbar. */
export function ActionToolbar({
  node,
  ariaLabel = "Node actions",
}: ActionToolbarProps): JSX.Element {
  const openModal = useUiStore((s) => s.openModal);
  const { run } = useActions();
  const [checkMenu, setCheckMenu] = useState(false);
  const checkBtnRef = useRef<HTMLDivElement>(null);

  if (!node) {
    return (
      <div className="spork-toolbar" role="toolbar" aria-label={ariaLabel}>
        <span className="spork-toolbar-empty">Select a node to act on it</span>
      </div>
    );
  }

  function trigger(action: ToolbarAction): void {
    if (!node) return;
    switch (action.id) {
      case "restore":
        openModal({ kind: "restore", nodeId: node.id });
        break;
      case "newBranch":
        openModal({ kind: "newBranch", nodeId: node.id });
        break;
      case "merge":
        openModal({ kind: "merge", nodeId: node.id });
        break;
      case "commit":
        openModal({ kind: "commit", nodeId: node.id });
        break;
      case "push":
        openModal({ kind: "push", nodeId: node.id });
        break;
      case "runCheck":
        setCheckMenu((v) => !v);
        break;
    }
  }

  async function runCheck(kind: string): Promise<void> {
    setCheckMenu(false);
    if (!node) return;
    await run(
      { command: "NODE_RUN_CHECK", targetNodeId: node.id, spec: { kind } },
      { nodeId: node.id, label: `${kind} check` },
    );
  }

  return (
    <div className="spork-toolbar" role="toolbar" aria-label={ariaLabel}>
      {TOOLBAR_ACTIONS.map((a) => {
        const enabled = a.enabledWhen(node);
        if (a.id === "runCheck") {
          return (
            <div
              key={a.id}
              ref={checkBtnRef}
              style={{ position: "relative", display: "inline-flex" }}
            >
              <Button
                variant={a.variant ?? "secondary"}
                size="sm"
                icon={a.icon}
                disabled={!enabled}
                data-action={a.id}
                aria-haspopup="menu"
                aria-expanded={checkMenu}
                onClick={() => trigger(a)}
              >
                {a.label}
                <Icon name="chevron-down" size={12} />
              </Button>
              {checkMenu && enabled && (
                <div
                  className="spork-ctxmenu"
                  role="menu"
                  style={{ position: "absolute", top: "100%", left: 0, marginTop: 4 }}
                >
                  {CHECK_KINDS.map((c) => (
                    <button
                      key={c.kind}
                      className="spork-ctx-item"
                      role="menuitem"
                      data-check={c.kind}
                      onClick={() => void runCheck(c.kind)}
                    >
                      <Icon name="play" size={13} />
                      {c.label}
                    </button>
                  ))}
                </div>
              )}
            </div>
          );
        }
        return (
          <Button
            key={a.id}
            variant={a.variant ?? "secondary"}
            size="sm"
            icon={a.icon}
            disabled={!enabled}
            data-action={a.id}
            onClick={() => trigger(a)}
          >
            {a.label}
          </Button>
        );
      })}
    </div>
  );
}
