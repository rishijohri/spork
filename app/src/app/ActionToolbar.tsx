// The node-action toolbar (UI_UX_DESIGN.md §5.1, §7.1; REALIGNMENT_PLAN §5a).
//
// A SINGLE instance, in the top bar, gated by the selected node (§14.2/§14.5).
// Reframed for the timeline-first model: a primary "+ New node from here ▾" menu
// (Agentic / Action groups) creates a node from the selection, and the two direct
// node-ops (Restore, Merge into line) sit beside it. Branching is automatic —
// there is no "new branch" verb. Agentic items open the Ask modal pre-set to
// their intent; action checks dispatch directly; commit/push open their modals.

import { useRef, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import {
  NEW_NODE_GROUPS,
  NODE_OPS,
  type NewNodeItem,
  type NodeOp,
} from "./toolbar";
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
  const [newMenu, setNewMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  if (!node) {
    return (
      <div className="spork-toolbar" role="toolbar" aria-label={ariaLabel}>
        <span className="spork-toolbar-empty">Select a node to act on it</span>
      </div>
    );
  }

  const target = node;

  async function dispatchNewNode(item: NewNodeItem): Promise<void> {
    setNewMenu(false);
    switch (item.dispatch.type) {
      case "askAgent":
        openModal({ kind: "askAgent", nodeId: target.id, intent: item.dispatch.intent });
        break;
      case "check":
        await run(
          { command: "NODE_RUN_CHECK", targetNodeId: target.id, spec: { kind: item.dispatch.checkKind } },
          { nodeId: target.id, label: `${item.dispatch.checkKind} check` },
        );
        break;
      case "modal":
        openModal({ kind: item.dispatch.modal, nodeId: target.id });
        break;
    }
  }

  function triggerOp(op: NodeOp): void {
    switch (op.id) {
      case "restore":
        openModal({ kind: "restore", nodeId: target.id });
        break;
      case "merge":
        openModal({ kind: "merge", nodeId: target.id });
        break;
    }
  }

  return (
    <div className="spork-toolbar" role="toolbar" aria-label={ariaLabel}>
      <div ref={menuRef} style={{ position: "relative", display: "inline-flex" }}>
        <Button
          variant="primary"
          size="sm"
          icon="plus"
          data-action="newNode"
          aria-haspopup="menu"
          aria-expanded={newMenu}
          onClick={() => setNewMenu((v) => !v)}
        >
          New node
          <Icon name="chevron-down" size={12} />
        </Button>
        {newMenu && (
          <>
            <div
              onClick={() => setNewMenu(false)}
              style={{ position: "fixed", inset: 0, zIndex: 30 }}
              aria-hidden="true"
            />
            <div
              className="spork-ctxmenu spork-newnode-menu"
              role="menu"
              aria-label="New node from here"
              style={{ position: "absolute", top: "100%", left: 0, marginTop: 4, zIndex: 31 }}
            >
              <span className="spork-ctx-eyebrow">New node from here</span>
              {NEW_NODE_GROUPS.map((group) => (
                <div key={group.label} className="spork-newnode-group">
                  <span className="spork-ctx-grouplabel">{group.label}</span>
                  {group.items.map((item) => {
                    const enabled = item.enabledWhen(target);
                    return (
                      <button
                        key={item.id}
                        className="spork-ctx-item spork-newnode-item"
                        role="menuitem"
                        data-newnode={item.id}
                        disabled={!enabled}
                        onClick={() => void dispatchNewNode(item)}
                      >
                        <Icon name={item.icon} size={14} />
                        <span className="spork-newnode-text">
                          <span className="spork-newnode-label">{item.label}</span>
                          <span className="spork-newnode-help">{item.help}</span>
                        </span>
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {NODE_OPS.map((op) => (
        <Button
          key={op.id}
          variant={op.variant ?? "secondary"}
          size="sm"
          icon={op.icon}
          disabled={!op.enabledWhen(target)}
          data-action={op.id}
          onClick={() => triggerOp(op)}
        >
          {op.label}
        </Button>
      ))}
    </div>
  );
}
