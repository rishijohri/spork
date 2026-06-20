// Left navigator + legend (DESIGN.md §14.2, §14.5).
//
// A schema-driven legend of node types and their colors/icons, rendered off the
// same descriptor map the canvas uses — so the legend and the cards never drift.

import { allDescriptors } from "../canvas/descriptors";

/** The left-region legend listing every known node type. */
export function Legend(): JSX.Element {
  const descriptors = allDescriptors();
  return (
    <aside className="spork-legend" aria-label="Node type legend">
      <h2 className="spork-legend-title">Node types</h2>
      <ul className="spork-legend-list">
        {descriptors.map((d) => (
          <li key={d.kind} className="spork-legend-item">
            <span
              className="spork-legend-swatch"
              style={{ background: d.color }}
              aria-hidden="true"
            />
            <span className="spork-legend-icon" aria-hidden="true">
              {d.icon}
            </span>
            <span className="spork-legend-label">{d.label}</span>
          </li>
        ))}
      </ul>
    </aside>
  );
}
