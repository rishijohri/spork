// Status sub-strip (UI_UX_DESIGN.md §4, §5.9, §5.12; REALIGNMENT_PLAN §5a).
//
// A thin always-visible band beneath the rail: daemon connection, current
// **line** (not a branch — informational, non-clickable), node count, the
// per-line spend, and the current selection. Stays visible even when the rail is
// collapsed. When disconnected it offers a Retry.

import type { JSX } from "react";
import { useUiStore } from "../state/store";
import { shortId, formatMicroUsd } from "../ui/format";
import { lineOfNode } from "../state/lines";

const CONN_LABEL: Record<string, string> = {
  connected: "connected",
  reconnecting: "reconnecting…",
  disconnected: "disconnected",
  "no-project": "no project open",
  mock: "mock mode",
};

/** The bottom status sub-strip. */
export function StatusStrip({ onRetry }: { onRetry?: () => void }): JSX.Element {
  const connection = useUiStore((s) => s.connection);
  const view = useUiStore((s) => s.view);
  const selectedId = useUiStore((s) => s.selectedNodeId);

  // The current line: the selected node's line, else the HEAD's, else main.
  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const currentLine =
    (selectedId && lineOfNode(view, selectedId)) ||
    (headTarget && lineOfNode(view, headTarget)) ||
    null;
  const lineLabel = currentLine?.label ?? "main";
  const lineBranchId = currentLine?.branchId ?? "main";

  // The per-line cost ledger (P6, §12.5): sum the priced cost of every node on
  // the current line. Zero/absent costs contribute nothing.
  const branchMicroUsd = view.nodes.reduce(
    (sum, n) =>
      n.branchId === lineBranchId && n.cost ? sum + n.cost.microUsd : sum,
    0,
  );

  return (
    <footer className="spork-substrip" aria-label="Status">
      <span className="spork-conn" data-state={connection}>
        <span className="spork-conn-dot" aria-hidden="true" />
        {CONN_LABEL[connection] ?? connection}
      </span>
      {connection === "disconnected" && onRetry && (
        <button className="btn btn--sm" onClick={onRetry}>
          Retry
        </button>
      )}
      <span className="spork-substrip-sep">·</span>
      <span title="The current line — an emergent line of work, not a git branch">
        line <span className="mono">{lineLabel}</span>
      </span>
      <span className="spork-substrip-sep">·</span>
      <span>
        <span className="mono">{view.nodes.length}</span> nodes
      </span>
      <span className="spork-substrip-sep">·</span>
      <span title="Total priced model spend on this line (P6 cost ledger)">
        spend <span className="mono">{formatMicroUsd(branchMicroUsd)}</span>
      </span>
      <span className="spork-substrip-sep">·</span>
      <span>
        sel <span className="mono">{shortId(selectedId)}</span>
      </span>
    </footer>
  );
}
