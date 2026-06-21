// Schema-driven node-type descriptors (DESIGN.md §14.5).
//
// Each node type ships a descriptor (color, icon, label, family) so the node
// card, legend, and details panel render off DATA, not a per-type frontend
// release — and the six built-ins use the SAME contract a user-defined type
// would (DESIGN.md §14.5: "a new node type is data, not a frontend release").
//
// The authoritative source of color/icon/displayName is the daemon's
// `NodeTypeDescriptor.ui_contributions` — an opaque JSON value the F2 registry
// stores per type (crates/spork-registry/src/lib.rs). The six built-ins set it
// to `{ "color": "#hex", "icon": "name", "displayName": "Name" }`
// (crates/spork-nodes/src/{edit,snapshot,sanity,merge,stress,validation}.rs).
// `descriptorFromUiContributions` parses exactly that shape, so once a slice
// wires the daemon's descriptor map through IPC, custom types render IDENTICALLY
// to built-ins.
//
// The `BUILTIN_DESCRIPTORS` map below is the offline default: it mirrors the
// frozen Rust `ui_contributions` byte-for-byte (verified against spork-nodes),
// so the canvas renders correct, consistent cards even before any daemon
// descriptor is loaded. Any unknown kind falls back to a neutral descriptor
// (forward-tolerant — the canvas never breaks on a new/custom type).
//
// Built-in kinds are verified against crates/spork-nodes (the `*_KIND` consts):
//   codebase-edit, validation, stress, sanity, merge, snapshot.

import type { Family } from "../ipc/types";

/** The renderer-facing descriptor for one node type. */
export interface NodeTypeDescriptor {
  /** The node `kind` discriminator from the view-model. */
  kind: string;
  /** Human-facing label shown on the card and in the legend. */
  label: string;
  /** Card accent color (CSS color string). */
  color: string;
  /** A short icon glyph (legacy fallback; the SVG icon set uses `iconName`). */
  icon: string;
  /**
   * The icon NAME from `ui_contributions.icon` (e.g. "pencil"), resolved by the
   * SVG icon set (src/ui/icons.tsx). This is the schema-driven icon identity; an
   * unknown name falls back to the neutral circle icon.
   */
  iconName: string;
  /** The node family this kind belongs to (drives some toolbar gating). */
  family: Family;
}

/**
 * The default `ui_contributions` for the six P5 built-in node types, mirroring
 * the frozen Rust values in `crates/spork-nodes` (the `ui_contributions:
 * json!({ "color": ..., "icon": ..., "displayName": ... })` literals). Keeping
 * these in lockstep means the offline default renders the same card the daemon
 * would drive (DESIGN.md §14.5).
 */
const BUILTIN_UI_CONTRIBUTIONS: Readonly<
  Record<string, { color: string; icon: string; displayName: string; family: Family }>
> = {
  "codebase-edit": {
    color: "#22c55e",
    icon: "pencil",
    displayName: "Edit",
    family: "mutating",
  },
  snapshot: {
    color: "#4f8cff",
    icon: "camera",
    displayName: "Snapshot",
    family: "mutating",
  },
  merge: {
    color: "#ec4899",
    icon: "git-merge",
    displayName: "Merge",
    family: "mutating",
  },
  sanity: {
    color: "#f59e0b",
    icon: "shield-check",
    displayName: "Sanity",
    family: "observing",
  },
  validation: {
    color: "#3b82f6",
    icon: "check-circle",
    displayName: "Validation",
    family: "observing",
  },
  stress: {
    color: "#a855f7",
    icon: "activity",
    displayName: "Stress",
    family: "observing",
  },
  // P6: the read-only agent run attaches its answer as a context node (DESIGN
  // §6.6). Matches the daemon's agent_context_descriptor ui_contributions.
  "agent-context": {
    color: "#a78bfa",
    icon: "messages-square",
    displayName: "Agent",
    family: "context",
  },
};

/**
 * Map an `ui_contributions` icon NAME (the Rust value, e.g. `"pencil"`) to a
 * short glyph the minimal card renders. A real icon set replaces this without a
 * descriptor change; an unknown name falls back to a neutral dot.
 */
const ICON_GLYPHS: Readonly<Record<string, string>> = {
  pencil: "✎",
  camera: "▣",
  "git-merge": "⤳",
  "shield-check": "◎",
  "check-circle": "✓",
  activity: "⚡",
};

/** Resolve an icon name to its glyph (forward-tolerant). */
export function iconGlyph(name: string | undefined): string {
  if (name === undefined) return "○";
  return ICON_GLYPHS[name] ?? "○";
}

/** A neutral fallback for an unknown (e.g. future/custom) node kind. */
function fallbackDescriptor(kind: string): NodeTypeDescriptor {
  return {
    kind,
    label: kind,
    color: "#9ca3af",
    icon: "○",
    iconName: "circle",
    family: "context",
  };
}

/**
 * Build a descriptor for `kind` from a daemon `ui_contributions` JSON value (the
 * opaque object on `NodeTypeDescriptor.ui_contributions`). Recognizes the
 * `{ color, icon, displayName }` shape the built-ins and any well-formed custom
 * type use; missing/wrong-typed fields fall back to the neutral defaults, so a
 * malformed contribution degrades gracefully rather than throwing.
 *
 * This is the SCHEMA-DRIVEN entry point: a future slice fetches the daemon's
 * descriptor map and calls this per kind so built-in and custom types render
 * identically (DESIGN.md §14.5).
 */
export function descriptorFromUiContributions(
  kind: string,
  uiContributions: unknown,
  family?: Family,
): NodeTypeDescriptor {
  const base = fallbackDescriptor(kind);
  if (uiContributions === null || typeof uiContributions !== "object") {
    return family ? { ...base, family } : base;
  }
  const ui = uiContributions as Record<string, unknown>;
  const color = typeof ui["color"] === "string" ? (ui["color"] as string) : base.color;
  const iconName = typeof ui["icon"] === "string" ? (ui["icon"] as string) : undefined;
  const label =
    typeof ui["displayName"] === "string" ? (ui["displayName"] as string) : base.label;
  return {
    kind,
    label,
    color,
    icon: iconGlyph(iconName),
    iconName: iconName ?? base.iconName,
    family: family ?? base.family,
  };
}

/** The default descriptors for the six P5 built-in node types. */
export const BUILTIN_DESCRIPTORS: Readonly<Record<string, NodeTypeDescriptor>> =
  Object.fromEntries(
    Object.entries(BUILTIN_UI_CONTRIBUTIONS).map(([kind, ui]) => [
      kind,
      descriptorFromUiContributions(kind, ui, ui.family),
    ]),
  );

/**
 * Resolve a node `kind` to its descriptor.
 *
 * Forward-tolerant: an unrecognized kind (a custom or future type) gets a
 * neutral descriptor rather than throwing, so the canvas never breaks on a new
 * type (DESIGN.md §14.5 — "a new node type is data, not a frontend release").
 */
export function descriptorFor(kind: string): NodeTypeDescriptor {
  return BUILTIN_DESCRIPTORS[kind] ?? fallbackDescriptor(kind);
}

/** Every known descriptor, for rendering the legend. */
export function allDescriptors(): NodeTypeDescriptor[] {
  return Object.values(BUILTIN_DESCRIPTORS);
}
