// Status sub-strip (UI_UX_DESIGN.md §4, §5.9, §5.12).
//
// A thin always-visible band beneath the rail: daemon connection, current
// branch, node count, and the current selection. Stays visible even when the
// rail is collapsed — the persistent "is it connected / where am I" context the
// first cut lacked. When disconnected it offers a Retry.

import type { JSX } from "react";
import { useUiStore } from "../state/store";
import { shortId } from "../ui/format";

const CONN_LABEL: Record<string, string> = {
  connected: "connected",
  reconnecting: "reconnecting…",
  disconnected: "disconnected",
  mock: "mock mode",
};

/** The bottom status sub-strip. */
export function StatusStrip({ onRetry }: { onRetry?: () => void }): JSX.Element {
  const connection = useUiStore((s) => s.connection);
  const view = useUiStore((s) => s.view);
  const selectedId = useUiStore((s) => s.selectedNodeId);

  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const currentBranch =
    view.refs.find((r) => r.kind === "Branch" && r.target === headTarget)?.name ??
    "main";

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
      <span>
        branch <span className="mono">{currentBranch}</span>
      </span>
      <span className="spork-substrip-sep">·</span>
      <span>
        <span className="mono">{view.nodes.length}</span> nodes
      </span>
      <span className="spork-substrip-sep">·</span>
      <span>
        sel <span className="mono">{shortId(selectedId)}</span>
      </span>
    </footer>
  );
}
