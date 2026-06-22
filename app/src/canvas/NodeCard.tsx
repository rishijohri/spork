// The custom React Flow node card (UI_UX_DESIGN.md §5.3, §6).
//
// A schema-driven, information-rich card replacing the bare "glyph + label"
// default node. It renders ONLY fields present in the view-model today (§5.3):
// type color stripe + icon, kind label, the derived effectiveStatus badge,
// branch + model chips, a snapshot dot, and a selection ring. Title/diff-stat/
// stale-reason are forward-map (additive view fields) and intentionally absent.

import { memo, type JSX } from "react";
import { Handle, Position } from "@xyflow/react";
import type { NodeView } from "../ipc/types";
import type { NodeTypeDescriptor } from "./descriptors";
import { Icon } from "../ui/icons";
import { humanizeModel, statusBadge } from "../ui/format";

/** The data the canvas attaches to each React Flow node. */
export interface NodeCardData {
  node: NodeView;
  descriptor: NodeTypeDescriptor;
  selected: boolean;
  dimmed: boolean;
  [key: string]: unknown;
}

const HANDLE_STYLE = { opacity: 0, width: 1, height: 1, border: "none" } as const;

/** A rich, schema-driven node card. */
function NodeCardImpl({ data }: { data: NodeCardData }): JSX.Element {
  const { node, descriptor, selected, dimmed } = data;
  const badge = statusBadge(node);

  return (
    <div
      className={`spork-card${selected ? " spork-card--selected" : ""}`}
      style={dimmed ? { opacity: 0.3 } : undefined}
      data-kind={node.kind}
      data-testid="node-card"
    >
      <Handle type="target" position={Position.Left} style={HANDLE_STYLE} />
      <span
        className="spork-card-stripe"
        style={{ background: descriptor.color }}
        aria-hidden="true"
      />
      <div className="spork-card-body">
        <div className="spork-card-top">
          <span
            className="spork-card-icon"
            style={{ color: descriptor.color }}
            aria-hidden="true"
          >
            <Icon name={descriptor.iconName} size={15} />
          </span>
          <span className="spork-card-kind">{descriptor.label}</span>
          <span
            className="spork-status"
            data-status={badge.tone}
            data-stale={badge.stale ? "true" : undefined}
            title={badge.stale ? `${badge.label} · stale` : badge.label}
          >
            <span className="spork-status-dot" aria-hidden="true" />
            {badge.label}
            {badge.stale && <span className="spork-stale-tag"> · stale</span>}
          </span>
        </div>
        {/* Lane membership is shown by the node's swimlane position, so the raw
            branchId chip is gone (REALIGNMENT_PLAN §5a) — only the model chip
            remains in the meta row. */}
        {node.model && (
          <div className="spork-card-meta">
            <span className="spork-chip" title={node.model}>
              ◆ {humanizeModel(node.model)}
            </span>
          </div>
        )}
      </div>
      {node.ownsSnapshot && (
        <span
          className="spork-snap-dot"
          title="Owns a restorable snapshot"
          aria-label="owns snapshot"
        />
      )}
      <Handle type="source" position={Position.Right} style={HANDLE_STYLE} />
    </div>
  );
}

export const NodeCard = memo(NodeCardImpl);
