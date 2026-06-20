// Bottom status / run rail (DESIGN.md §14.2, §14.4).
//
// Two streams surface here:
//   1. The ACTIVITY LOG — a bounded, newest-last list of every toolbar action's
//      outcome (and any error), so actions that create no canvas node (Commit /
//      Push / New Branch / Restore) still give the user visible feedback instead
//      of silence. Error lines are styled distinctly (red), success greenish,
//      info muted; each carries a timestamp. This is an accessible log region.
//   2. The EPHEMERAL FRAMES — live test/agent output streamed via the side-
//      channels for the SELECTED node (`ingestEphemeral`), kept as before.

import { useUiStore } from "../state/store";
import type { ActivityEntry } from "../state/store";

/** Format an activity timestamp as a compact local HH:MM:SS. */
function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** The bottom run/status rail: the activity log plus the selected node's stream. */
export function RunRail(): JSX.Element {
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const rail = useUiStore((s) => s.rail);
  const activity = useUiStore((s) => s.activity);

  const lines = selectedId ? (rail[selectedId] ?? []) : [];

  return (
    <footer className="spork-runrail" aria-label="Run output">
      <div className="spork-runrail-header">
        <span>Run output</span>
        {selectedId && <code className="spork-runrail-node">{selectedId}</code>}
      </div>

      <ul
        className="spork-activity-log"
        data-testid="activity-log"
        role="log"
        aria-label="Activity log"
        aria-live="polite"
      >
        {activity.length === 0 ? (
          <li className="spork-muted spork-activity-empty">No activity yet.</li>
        ) : (
          activity.map((entry) => <ActivityLine key={entry.id} entry={entry} />)
        )}
      </ul>

      <pre className="spork-runrail-log" data-testid="runrail-log">
        {lines.join("")}
      </pre>
    </footer>
  );
}

/** One activity-log line, styled by its severity level. */
function ActivityLine({ entry }: { entry: ActivityEntry }): JSX.Element {
  return (
    <li
      className={`spork-activity-line spork-activity-${entry.level}`}
      data-level={entry.level}
      data-testid="activity-entry"
    >
      <time className="spork-activity-ts" dateTime={new Date(entry.ts).toISOString()}>
        {formatTime(entry.ts)}
      </time>
      <span className="spork-activity-text">{entry.text}</span>
    </li>
  );
}
