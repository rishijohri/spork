// Bottom run rail (UI_UX_DESIGN.md §5.9, §7.5).
//
// v1 rail = the ACTIVITY tab only (op-log + action outcomes) — the real,
// producing stream. The "Run output" tab is forward-map (🟡): nothing streams
// onto RUN_STDOUT yet, so it renders as a disabled, labelled tab rather than a
// dead live pane. Each activity line pairs a severity ICON with color (not
// color-only — accessibility) and a timestamp. Collapsible; copy/clear per tab.

import type { JSX } from "react";
import { useUiStore } from "../state/store";
import type { ActivityEntry, ActivityLevel } from "../state/store";
import { IconButton } from "../ui/Button";
import { Icon, type IconName } from "../ui/icons";

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

const LEVEL_ICON: Record<ActivityLevel, IconName> = {
  info: "info",
  success: "check",
  error: "x-circle",
};

/** The bottom status/run rail (region 5): the Activity stream. */
export function RunRail(): JSX.Element {
  const activity = useUiStore((s) => s.activity);
  const railCollapsed = useUiStore((s) => s.railCollapsed);
  const setRailCollapsed = useUiStore((s) => s.setRailCollapsed);

  function clear(): void {
    useUiStore.setState({ activity: [] });
  }
  function copy(): void {
    const text = activity
      .map((e) => `${formatTime(e.ts)}  ${e.text}`)
      .join("\n");
    try {
      void navigator.clipboard?.writeText(text);
    } catch {
      /* clipboard unavailable — ignore */
    }
  }

  return (
    <section className="spork-rail" data-collapsed={railCollapsed} aria-label="Run rail">
      <div className="spork-rail-head">
        <button className="spork-rail-tab" role="tab" aria-selected={true}>
          Activity
        </button>
        <button
          className="spork-rail-tab"
          role="tab"
          aria-selected={false}
          disabled
          title="Streamed run output arrives in P7"
        >
          Run output
        </button>
        <div className="spork-rail-spacer" />
        <IconButton icon="copy" label="Copy activity" size={14} onClick={copy} />
        <IconButton icon="trash" label="Clear activity" size={14} onClick={clear} />
        <IconButton
          icon={railCollapsed ? "chevron-up" : "chevron-down"}
          label={railCollapsed ? "Expand rail" : "Collapse rail"}
          size={14}
          onClick={() => setRailCollapsed(!railCollapsed)}
        />
      </div>

      {!railCollapsed &&
        (activity.length === 0 ? (
          <div className="spork-activity-empty">No activity yet.</div>
        ) : (
          <ul
            className="spork-activity"
            data-testid="activity-log"
            role="log"
            aria-label="Activity log"
            aria-live="polite"
          >
            {activity.map((e) => (
              <ActivityLine key={e.id} entry={e} />
            ))}
          </ul>
        ))}
    </section>
  );
}

function ActivityLine({ entry }: { entry: ActivityEntry }): JSX.Element {
  return (
    <li
      className="spork-activity-line"
      data-level={entry.level}
      data-testid="activity-entry"
    >
      <span className="spork-activity-ico" aria-hidden="true">
        <Icon name={LEVEL_ICON[entry.level]} size={12} />
      </span>
      <time className="spork-activity-ts" dateTime={new Date(entry.ts).toISOString()}>
        {formatTime(entry.ts)}
      </time>
      <span className="spork-activity-text">{entry.text}</span>
    </li>
  );
}
