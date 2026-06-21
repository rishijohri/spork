// Left navigator + legend (UI_UX_DESIGN.md §5.2, §7.2).
//
// Real navigation, not just a static legend: a Branches list from the view-model
// refs (current branch highlighted, right-click for set-HEAD / new-branch /
// merge), and a node-type legend whose rows double as a canvas highlight filter
// (with live per-type counts). Collapsible to a thin rail to give the canvas room.
// Everything here is schema-driven (descriptors) and backed by view-model reads.

import { useMemo, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { allDescriptors, descriptorFor } from "../canvas/descriptors";
import { Icon } from "../ui/icons";
import { IconButton } from "../ui/Button";
import { shortId } from "../ui/format";
import type { RefView } from "../ipc/types";

/** The left navigator region. */
export function Navigator(): JSX.Element {
  const view = useUiStore((s) => s.view);
  const navCollapsed = useUiStore((s) => s.navCollapsed);
  const toggleNav = useUiStore((s) => s.toggleNav);
  const kindFilter = useUiStore((s) => s.kindFilter);
  const toggleKindFilter = useUiStore((s) => s.toggleKindFilter);
  const selectNode = useUiStore((s) => s.selectNode);
  const selectedNodeId = useUiStore((s) => s.selectedNodeId);
  const openModal = useUiStore((s) => s.openModal);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const [typeQuery, setTypeQuery] = useState("");

  // Per-kind counts from the live view-model.
  const counts = useMemo(() => {
    const m: Record<string, number> = {};
    for (const n of view.nodes) m[n.kind] = (m[n.kind] ?? 0) + 1;
    return m;
  }, [view.nodes]);

  // Branch/tag refs + the current HEAD target (to mark the active branch).
  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const branches = view.refs.filter((r) => r.kind === "Branch" || r.kind === "Tag");

  const descriptors = allDescriptors().filter(
    (d) =>
      !typeQuery ||
      d.label.toLowerCase().includes(typeQuery.toLowerCase()) ||
      d.kind.toLowerCase().includes(typeQuery.toLowerCase()),
  );

  if (navCollapsed) {
    return (
      <nav className="spork-nav spork-nav--collapsed" aria-label="Navigator">
        <div className="spork-nav-scroll" style={{ alignItems: "center", display: "flex", flexDirection: "column", gap: 8 }}>
          <IconButton icon="panel-left" label="Expand navigator" onClick={toggleNav} />
          {allDescriptors().map((d) => (
            <span
              key={d.kind}
              className="spork-swatch"
              style={{ background: d.color }}
              title={`${d.label} (${counts[d.kind] ?? 0})`}
            />
          ))}
        </div>
      </nav>
    );
  }

  function onBranchContext(e: React.MouseEvent, ref: RefView): void {
    e.preventDefault();
    openContextMenu({
      kind: "branch",
      ref: ref.name,
      target: ref.target,
      x: e.clientX,
      y: e.clientY,
    });
  }

  return (
    <nav className="spork-nav" aria-label="Navigator">
      <div className="spork-nav-scroll">
        {/* Branches */}
        <section className="spork-nav-section" aria-label="Branches">
          <div className="spork-nav-sectionhdr">
            <span className="spork-eyebrow">Branches</span>
            <IconButton
              icon="plus"
              label="New branch"
              size={14}
              onClick={() =>
                openModal({
                  kind: "newBranch",
                  nodeId: selectedNodeId ?? headTarget ?? "",
                })
              }
            />
          </div>
          {branches.length === 0 ? (
            <p className="spork-faint" style={{ padding: "2px 8px", fontSize: 12 }}>
              No branches yet.
            </p>
          ) : (
            branches.map((r) => {
              const current = headTarget !== null && r.target === headTarget;
              return (
                <button
                  key={r.name}
                  className={`spork-row${current ? " spork-row--active" : ""}`}
                  onClick={() => selectNode(r.target)}
                  onContextMenu={(e) => onBranchContext(e, r)}
                  title={`${r.name} → ${shortId(r.target)}`}
                >
                  <Icon name="git-branch" size={13} />
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                    {r.name}
                  </span>
                  {current && (
                    <span className="spork-row-count" aria-label="current">
                      HEAD
                    </span>
                  )}
                </button>
              );
            })
          )}
        </section>

        {/* Node types (legend + filter) */}
        <section className="spork-nav-section" aria-label="Node types">
          <div className="spork-nav-sectionhdr">
            <span className="spork-eyebrow">Node types</span>
            {kindFilter.length > 0 && (
              <IconButton
                icon="x"
                label="Clear type filter"
                size={14}
                onClick={() => useUiStore.getState().clearKindFilter()}
              />
            )}
          </div>
          <div className="spork-field" style={{ margin: "0 4px 8px" }}>
            <input
              type="search"
              value={typeQuery}
              onChange={(e) => setTypeQuery(e.target.value)}
              placeholder="Filter types…"
              aria-label="Filter node types"
            />
          </div>
          {descriptors.map((d) => {
            const active = kindFilter.includes(d.kind);
            const dim = kindFilter.length > 0 && !active;
            return (
              <button
                key={d.kind}
                className={`spork-row${active ? " spork-row--active" : ""}${dim ? " spork-row--dim" : ""}`}
                aria-pressed={active}
                onClick={() => toggleKindFilter(d.kind)}
                title={`Filter to ${d.label}`}
              >
                <span className="spork-swatch" style={{ background: d.color }} />
                <span style={{ color: d.color, display: "inline-flex" }}>
                  <Icon name={d.iconName} size={13} />
                </span>
                <span>{d.label}</span>
                <span className="spork-row-count">{counts[d.kind] ?? 0}</span>
              </button>
            );
          })}
        </section>
      </div>
      <div style={{ borderTop: "1px solid var(--border-subtle)", padding: 4 }}>
        <IconButton icon="panel-left" label="Collapse navigator" onClick={toggleNav} />
      </div>
    </nav>
  );
}

// Re-export the descriptor lookup for any legend consumer that wants it.
export { descriptorFor };
