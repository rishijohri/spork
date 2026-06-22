// Left navigator + legend (UI_UX_DESIGN.md §5.2, §7.2; REALIGNMENT_PLAN §5a).
//
// Real navigation, not a static legend: a **LINES** overview (the emergent lines
// of the timeline, derived by grouping nodes by their internal `branchId`) and a
// node-type legend whose rows double as a canvas highlight filter (with live
// per-type counts). Clicking a line **focuses** its lane (dims the others on the
// canvas) and selects its tip — it never switches HEAD; lines are emergent, not
// managed. There is no "new branch" — branching is automatic. Collapsible to a
// thin rail. Everything is schema-driven (descriptors) over view-model reads.

import { useMemo, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { descriptorFor, legendDescriptors } from "../canvas/descriptors";
import { deriveLines } from "../state/lines";
import { Icon } from "../ui/icons";
import { IconButton } from "../ui/Button";
import { ResizeHandle } from "../ui/ResizeHandle";

/** The left navigator region. */
export function Navigator(): JSX.Element {
  const view = useUiStore((s) => s.view);
  const navCollapsed = useUiStore((s) => s.navCollapsed);
  const toggleNav = useUiStore((s) => s.toggleNav);
  const kindFilter = useUiStore((s) => s.kindFilter);
  const toggleKindFilter = useUiStore((s) => s.toggleKindFilter);
  const selectNode = useUiStore((s) => s.selectNode);
  const focusedLane = useUiStore((s) => s.focusedLane);
  const focusLane = useUiStore((s) => s.focusLane);
  const navWidth = useUiStore((s) => s.navWidth);
  const setNavWidth = useUiStore((s) => s.setNavWidth);
  const [typeQuery, setTypeQuery] = useState("");

  // Per-kind counts from the live view-model.
  const counts = useMemo(() => {
    const m: Record<string, number> = {};
    for (const n of view.nodes) m[n.kind] = (m[n.kind] ?? 0) + 1;
    return m;
  }, [view.nodes]);

  // The emergent lines (lanes), derived from the node DAG (main first).
  const lines = useMemo(() => deriveLines(view), [view]);

  // Honest legend: the universal agentic creatables + only the kinds that
  // actually exist in the graph (contextual action/check types appear once real,
  // never as always-present maybe-meaningless entries — REALIGNMENT_PLAN §3a).
  const legend = useMemo(() => legendDescriptors(Object.keys(counts)), [counts]);
  const descriptors = legend.filter(
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
          {legend.map((d) => (
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

  /** Click a line: focus its lane (toggle) and select its tip. */
  function onLineClick(branchId: string, tipNodeId: string): void {
    focusLane(focusedLane === branchId ? null : branchId);
    selectNode(tipNodeId);
  }

  return (
    <nav className="spork-nav" aria-label="Navigator">
      <div className="spork-nav-scroll">
        {/* Lines */}
        <section className="spork-nav-section" aria-label="Lines">
          <div className="spork-nav-sectionhdr">
            <span className="spork-eyebrow">Lines</span>
            {focusedLane !== null && (
              <IconButton
                icon="x"
                label="Show all lines"
                size={14}
                onClick={() => focusLane(null)}
              />
            )}
          </div>
          {lines.length === 0 ? (
            <p className="spork-faint" style={{ padding: "2px 8px", fontSize: 12 }}>
              No lines yet.
            </p>
          ) : (
            lines.map((line) => {
              const active = focusedLane === line.branchId;
              const dim = focusedLane !== null && !active;
              return (
                <button
                  key={line.branchId}
                  className={`spork-row spork-line-row${active ? " spork-row--active" : ""}${dim ? " spork-row--dim" : ""}`}
                  aria-pressed={active}
                  onClick={() => onLineClick(line.branchId, line.tipNodeId)}
                  title={
                    line.forkedFrom
                      ? `${line.label} — forked line · ${line.count} node${line.count === 1 ? "" : "s"}`
                      : `${line.label} · ${line.count} node${line.count === 1 ? "" : "s"}`
                  }
                >
                  <span
                    className="spork-line-dot"
                    data-rollup={line.rollup}
                    aria-hidden="true"
                  />
                  <span className="spork-line-rowname" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                    {line.forkedFrom && <span className="spork-line-forkmark" aria-hidden="true">↳ </span>}
                    {line.label}
                  </span>
                  <span className="spork-row-count">{line.count}</span>
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
      <ResizeHandle
        axis="x"
        sign={1}
        value={navWidth}
        min={180}
        max={460}
        onChange={setNavWidth}
        label="Resize navigator"
      />
    </nav>
  );
}

// Re-export the descriptor lookup for any legend consumer that wants it.
export { descriptorFor };
