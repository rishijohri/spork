// Context menu (UI_UX_DESIGN.md §5.11, §7.2/§7.3; REALIGNMENT_PLAN §5a).
//
// A right-click menu for a canvas node. It mirrors the toolbar actions (single
// gating source) and routes through the same modals + action runner. There is no
// branch context menu — branching is automatic and lines are emergent, not
// managed. Forward-map items (Pin — P8) render as disabled rows labelled with
// their phase, never as dead live controls.

import { useEffect, useLayoutEffect, useRef, useState, type JSX } from "react";
import { useUiStore } from "../../state/store";
import { useActions } from "../useActions";
import { Icon } from "../../ui/icons";
import { isMaterializable } from "../toolbar";
import type { NodeView } from "../../ipc/types";

/** The right-click context menu host (reads the open menu from the store). */
export function ContextMenu(): JSX.Element | null {
  const menu = useUiStore((s) => s.contextMenu);
  const close = useUiStore((s) => s.closeContextMenu);
  const ref = useRef<HTMLDivElement>(null);
  // The raw right-click coords are the anchor; the rendered position is clamped to
  // the viewport so a menu opened near the right/bottom edge never clips off-screen
  // (the same off-screen class the Settings popover hit). Presentation-only — the
  // stored coords are not mutated.
  const [pos, setPos] = useState({ left: menu?.x ?? 0, top: menu?.y ?? 0 });

  // Reset to the raw anchor whenever a new menu opens, so it re-clamps fresh.
  useLayoutEffect(() => {
    if (menu) setPos({ left: menu.x, top: menu.y });
  }, [menu]);

  // After layout, clamp the measured menu box inside the viewport.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!menu || !el) return;
    const r = el.getBoundingClientRect();
    const left = Math.max(8, Math.min(menu.x, window.innerWidth - r.width - 8));
    const top = Math.max(8, Math.min(menu.y, window.innerHeight - r.height - 8));
    setPos((prev) => (prev.left === left && prev.top === top ? prev : { left, top }));
  }, [menu]);

  useEffect(() => {
    if (!menu) return;
    function onKey(e: KeyboardEvent): void {
      if (e.key === "Escape") close();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu, close]);

  if (!menu) return null;

  return (
    <>
      <div
        onClick={close}
        onContextMenu={(e) => {
          e.preventDefault();
          close();
        }}
        style={{ position: "fixed", inset: 0, zIndex: 55 }}
        aria-hidden="true"
      />
      <div
        ref={ref}
        className="spork-ctxmenu"
        role="menu"
        style={{ left: pos.left, top: pos.top }}
      >
        <NodeMenu nodeId={menu.nodeId} />
      </div>
    </>
  );
}

function Item({
  icon,
  label,
  onClick,
  disabled,
  hint,
}: {
  icon: string;
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  hint?: string;
}): JSX.Element {
  return (
    <button className="spork-ctx-item" role="menuitem" disabled={disabled} onClick={onClick}>
      <Icon name={icon} size={13} />
      {label}
      {hint && <span className="spork-ctx-shortcut">{hint}</span>}
    </button>
  );
}

function NodeMenu({ nodeId }: { nodeId: string }): JSX.Element {
  const view = useUiStore((s) => s.view);
  const openModal = useUiStore((s) => s.openModal);
  const close = useUiStore((s) => s.closeContextMenu);
  const { run } = useActions();
  const node: NodeView | null = view.nodes.find((n) => n.id === nodeId) ?? null;
  const mat = isMaterializable(node);
  const mutating = node?.family === "mutating";

  function act(fn: () => void): void {
    fn();
    close();
  }

  function copyId(): void {
    try {
      void navigator.clipboard?.writeText(nodeId);
    } catch {
      /* ignore */
    }
    close();
  }

  return (
    <>
      <Item
        icon="corner-up-left"
        label="Restore"
        disabled={!(mat && mutating)}
        onClick={() => act(() => openModal({ kind: "restore", nodeId }))}
      />
      <Item
        icon="messages-square"
        label="Ask agent…"
        onClick={() => act(() => openModal({ kind: "askAgent", nodeId }))}
      />
      <Item
        icon="play"
        label="Run validation"
        disabled={!mat}
        onClick={() =>
          act(() =>
            void run(
              { command: "NODE_RUN_CHECK", targetNodeId: nodeId, spec: { kind: "validation" } },
              { nodeId, label: "validation check" },
            ),
          )
        }
      />
      <Item icon="git-merge" label="Merge into line…" onClick={() => act(() => openModal({ kind: "merge", nodeId }))} />
      <Item icon="git-commit" label="Commit to Git" disabled={!mat} onClick={() => act(() => openModal({ kind: "commit", nodeId }))} />
      <Item icon="upload" label="Push" disabled={!mat} onClick={() => act(() => openModal({ kind: "push", nodeId }))} />
      <div className="spork-ctx-sep" />
      <Item icon="copy" label="Copy node id" onClick={copyId} />
      <Item
        icon="download"
        label="Check out node"
        disabled={!mat}
        onClick={() =>
          act(() =>
            void run(
              { command: "NODE_CHECKOUT", nodeId },
              { nodeId, label: "checkout (forks if non-tip)" },
            ),
          )
        }
      />
      <Item icon="circle-dot" label="Pin node" disabled hint="P8" />
    </>
  );
}
