// Context menu (UI_UX_DESIGN.md §5.11, §7.2/§7.3).
//
// A right-click menu for a canvas node or a navigator branch row. It mirrors the
// top-bar / navigator actions (single gating source) and routes through the same
// modals + action runner. Forward-map items (Check out, Pin — P7) render as
// disabled rows labelled with their phase, never as dead live controls.

import { useEffect, type JSX } from "react";
import { useUiStore } from "../../state/store";
import { useActions } from "../useActions";
import { Icon } from "../../ui/icons";
import { isMaterializable } from "../toolbar";
import type { NodeView } from "../../ipc/types";

/** The right-click context menu host (reads the open menu from the store). */
export function ContextMenu(): JSX.Element | null {
  const menu = useUiStore((s) => s.contextMenu);
  const close = useUiStore((s) => s.closeContextMenu);

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
        className="spork-ctxmenu"
        role="menu"
        style={{ left: menu.x, top: menu.y }}
      >
        {menu.kind === "node" ? (
          <NodeMenu nodeId={menu.nodeId} />
        ) : (
          <BranchMenu refName={menu.ref} target={menu.target} />
        )}
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
      <Item icon="git-branch" label="New branch" onClick={() => act(() => openModal({ kind: "newBranch", nodeId }))} />
      <Item icon="git-merge" label="Merge…" onClick={() => act(() => openModal({ kind: "merge", nodeId }))} />
      <Item icon="git-commit" label="Commit to Git" disabled={!mat} onClick={() => act(() => openModal({ kind: "commit", nodeId }))} />
      <Item icon="upload" label="Push" disabled={!mat} onClick={() => act(() => openModal({ kind: "push", nodeId }))} />
      <div className="spork-ctx-sep" />
      <Item icon="copy" label="Copy node id" onClick={copyId} />
      <Item icon="download" label="Check out node" disabled hint="P7" />
      <Item icon="circle-dot" label="Pin node" disabled hint="P7" />
    </>
  );
}

function BranchMenu({ refName, target }: { refName: string; target: string }): JSX.Element {
  const openModal = useUiStore((s) => s.openModal);
  const selectNode = useUiStore((s) => s.selectNode);
  const close = useUiStore((s) => s.closeContextMenu);
  const { run } = useActions();

  function act(fn: () => void): void {
    fn();
    close();
  }

  return (
    <>
      <Item
        icon="check-circle"
        label="Set as HEAD"
        onClick={() =>
          act(() =>
            void run({ command: "REF_MOVE", name: "HEAD", to: target }, { nodeId: target, label: "Set HEAD" }),
          )
        }
      />
      <Item icon="git-branch" label="New branch here" onClick={() => act(() => openModal({ kind: "newBranch", nodeId: target }))} />
      <Item icon="git-merge" label="Merge into…" onClick={() => act(() => openModal({ kind: "merge", nodeId: target }))} />
      <div className="spork-ctx-sep" />
      <Item icon="search" label="Focus lineage" onClick={() => act(() => selectNode(target))} />
      <span className="spork-ctx-item spork-faint" aria-disabled style={{ fontSize: 11 }}>
        {refName}
      </span>
    </>
  );
}
