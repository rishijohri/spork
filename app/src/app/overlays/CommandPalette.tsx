// Command palette (UI_UX_DESIGN.md §5.10f, §7.1).
//
// ⌘K fuzzy launcher. It exposes ONLY built (🟢) actions — the selected node's
// operations (which open the same modals) plus global undo/redo/GC — and a
// node-jump search. Nothing here dispatches an unbacked command. Keyboard:
// ↑/↓ move, Enter runs, Esc closes.

import { useMemo, useState, type JSX } from "react";
import { useUiStore } from "../../state/store";
import { useActions } from "../useActions";
import { Icon, type IconName } from "../../ui/icons";
import { descriptorFor } from "../../canvas/descriptors";
import { shortId } from "../../ui/format";
import { isMaterializable } from "../toolbar";
import type { NodeView } from "../../ipc/types";

interface Cmd {
  id: string;
  label: string;
  icon: IconName;
  run: () => void;
}

/** The ⌘K command palette. */
export function CommandPalette(): JSX.Element | null {
  const open = useUiStore((s) => s.paletteOpen);
  const setOpen = useUiStore((s) => s.setPaletteOpen);
  const view = useUiStore((s) => s.view);
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const openModal = useUiStore((s) => s.openModal);
  const selectNode = useUiStore((s) => s.selectNode);
  const setSettingsOpen = useUiStore((s) => s.setSettingsOpen);
  const { run } = useActions();
  const [q, setQ] = useState("");
  const [active, setActive] = useState(0);

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;
  const mat = isMaterializable(node);

  const commands = useMemo<Cmd[]>(() => {
    const list: Cmd[] = [];
    if (node) {
      if (mat && node.family === "mutating")
        list.push({ id: "restore", label: "Restore selected node", icon: "corner-up-left", run: () => openModal({ kind: "restore", nodeId: node.id }) });
      if (mat)
        list.push({ id: "check", label: "Run validation on selected node", icon: "play", run: () => void run({ command: "NODE_RUN_CHECK", targetNodeId: node.id, spec: { kind: "validation" } }, { nodeId: node.id, label: "validation check" }) });
      list.push({ id: "branch", label: "New branch from selected node", icon: "git-branch", run: () => openModal({ kind: "newBranch", nodeId: node.id }) });
      list.push({ id: "merge", label: "Merge from selected node", icon: "git-merge", run: () => openModal({ kind: "merge", nodeId: node.id }) });
      if (mat) {
        list.push({ id: "commit", label: "Commit selected node to Git", icon: "git-commit", run: () => openModal({ kind: "commit", nodeId: node.id }) });
        list.push({ id: "push", label: "Push selected node", icon: "upload", run: () => openModal({ kind: "push", nodeId: node.id }) });
      }
    }
    list.push({ id: "undo", label: "Undo", icon: "rotate-ccw", run: () => void run({ command: "OP_UNDO", opId: null }, { label: "Undo" }) });
    list.push({ id: "redo", label: "Redo", icon: "rotate-cw", run: () => void run({ command: "OP_REDO", opId: null }, { label: "Redo" }) });
    list.push({ id: "gc", label: "Garbage collection…", icon: "trash", run: () => openModal({ kind: "gc" }) });
    list.push({ id: "settings", label: "Open settings", icon: "settings", run: () => setSettingsOpen(true) });
    return list;
  }, [node, mat, openModal, run, setSettingsOpen]);

  const ql = q.trim().toLowerCase();
  const matched = ql ? commands.filter((c) => c.label.toLowerCase().includes(ql)) : commands;
  // Node-jump results when searching.
  const nodeHits: Cmd[] = ql
    ? view.nodes
        .filter((n) => {
          const d = descriptorFor(n.kind);
          return `${d.label} ${n.kind} ${n.id} ${n.branchId}`.toLowerCase().includes(ql);
        })
        .slice(0, 6)
        .map((n) => ({
          id: `go-${n.id}`,
          label: `Go to ${descriptorFor(n.kind).label} ${shortId(n.id)}`,
          icon: "search",
          run: () => selectNode(n.id),
        }))
    : [];
  const all = [...matched, ...nodeHits];

  if (!open) return null;

  function close(): void {
    setOpen(false);
    setQ("");
    setActive(0);
  }
  function runAt(i: number): void {
    const cmd = all[i];
    if (cmd) {
      cmd.run();
      close();
    }
  }

  return (
    <div
      className="spork-overlay"
      onMouseDown={(e) => e.target === e.currentTarget && close()}
    >
      <div className="spork-palette" role="dialog" aria-modal="true" aria-label="Command palette">
        <div className="spork-palette-input">
          <Icon name="search" size={16} />
          <input
            autoFocus
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setActive(0);
            }}
            placeholder="Run a command or jump to a node…"
            aria-label="Command palette input"
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setActive((i) => Math.min(i + 1, all.length - 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setActive((i) => Math.max(i - 1, 0));
              } else if (e.key === "Enter") {
                e.preventDefault();
                runAt(active);
              } else if (e.key === "Escape") {
                close();
              }
            }}
          />
          <span className="spork-kbd">esc</span>
        </div>
        {all.length === 0 ? (
          <div className="spork-palette-empty">No commands or nodes match “{q}”.</div>
        ) : (
          <ul className="spork-palette-list" role="listbox">
            {all.map((c, i) => (
              <li
                key={c.id}
                role="option"
                aria-selected={i === active}
                className={`spork-palette-item${i === active ? " spork-palette-item--active" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => runAt(i)}
              >
                <Icon name={c.icon} size={15} />
                {c.label}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
