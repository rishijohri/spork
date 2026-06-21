// Settings popover (UI_UX_DESIGN.md §5.13).
//
// The few real v1 preferences, all local renderer state (🟢): theme (dark-only in
// v1, light is a later token-swap), density, a panel-layout reset, and the
// default-model default. Engine-health / Extensions are forward-map (P8).

import type { JSX } from "react";
import { useUiStore } from "../../state/store";
import { MODELS } from "../TopBar";
import { humanizeModel } from "../../ui/format";

/** The settings popover body. */
export function SettingsPopover(): JSX.Element {
  const density = useUiStore((s) => s.density);
  const setDensity = useUiStore((s) => s.setDensity);
  const defaultModel = useUiStore((s) => s.defaultModel);
  const setDefaultModel = useUiStore((s) => s.setDefaultModel);

  function resetPanels(): void {
    const st = useUiStore.getState();
    if (st.navCollapsed) st.toggleNav();
    if (st.detailsCollapsed) st.toggleDetails();
    st.setRailCollapsed(false);
  }

  return (
    <div className="spork-popover" role="dialog" aria-label="Settings">
      <div className="spork-popover-group">
        <span className="spork-eyebrow">Appearance</span>
        <div className="spork-row-between">
          <span>Theme</span>
          <div className="spork-seg" role="group" aria-label="Theme">
            <button aria-pressed={true}>Dark</button>
            <button aria-pressed={false} disabled title="Light theme arrives later">
              Light
            </button>
          </div>
        </div>
        <div className="spork-row-between">
          <span>Density</span>
          <div className="spork-seg" role="group" aria-label="Density">
            <button
              aria-pressed={density === "comfortable"}
              onClick={() => setDensity("comfortable")}
            >
              Comfortable
            </button>
            <button
              aria-pressed={density === "compact"}
              onClick={() => setDensity("compact")}
            >
              Compact
            </button>
          </div>
        </div>
      </div>

      <div className="spork-popover-group">
        <span className="spork-eyebrow">Layout</span>
        <button className="btn" onClick={resetPanels}>
          Reset panels
        </button>
      </div>

      <div className="spork-popover-group">
        <span className="spork-eyebrow">Models</span>
        <div className="spork-row-between">
          <span>Default for new nodes</span>
          <select
            value={defaultModel}
            onChange={(e) => setDefaultModel(e.target.value)}
            aria-label="Default model for new nodes"
          >
            {MODELS.map((m) => (
              <option key={m} value={m}>
                {humanizeModel(m)}
              </option>
            ))}
          </select>
        </div>
      </div>

      <div className="spork-fwd-note">
        Engine health &amp; Extensions settings arrive in P8.
      </div>
    </div>
  );
}
