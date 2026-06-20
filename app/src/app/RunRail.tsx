// Bottom status / run rail (DESIGN.md §14.2, §14.4).
//
// Live test/agent output streams here via the ephemeral side-channels, keyed by
// node id. Frames are buffered into the store (`ingestEphemeral`) off the ordered
// op-log, so a flood never blocks durable updates. This region shows the rail for
// the selected node (or the most-recently-active node).

import { useUiStore } from "../state/store";

/** The bottom run/status rail for the selected node's ephemeral output. */
export function RunRail(): JSX.Element {
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const rail = useUiStore((s) => s.rail);

  const lines = selectedId ? (rail[selectedId] ?? []) : [];

  return (
    <footer className="spork-runrail" aria-label="Run output">
      <div className="spork-runrail-header">
        <span>Run output</span>
        {selectedId && <code className="spork-runrail-node">{selectedId}</code>}
      </div>
      <pre className="spork-runrail-log" data-testid="runrail-log">
        {lines.join("")}
      </pre>
    </footer>
  );
}
