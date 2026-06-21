// Shared display formatting (UI_UX_DESIGN.md §3.3, §6.2).
//
// Small pure helpers that turn raw view-model values into human-facing text:
// short ULIDs (never show the raw 26 chars), humanized edge/model labels, and the
// effective-status fold (status × isStale → one token, §6.2 / DESIGN §7.3).

import type { EdgeType, Lifecycle } from "../ipc/types";

/** A short, copyable form of a (long) ULID: the entropy-bearing tail. */
export function shortId(id: string | null | undefined): string {
  if (!id) return "—";
  return id.length > 8 ? `…${id.slice(-7)}` : id;
}

/** Humanize a model id: drop the provider prefix, keep it compact. */
export function humanizeModel(model: string | null): string {
  if (!model) return "—";
  const bare = model.includes("/") ? model.split("/").pop()! : model;
  return bare;
}

/**
 * Format an integer micro-USD spend as a compact dollar string (P6 cost ledger,
 * §12.5). `0` → "free"; otherwise a `$`-prefixed amount with enough precision to
 * show sub-cent spends a single local turn can incur.
 */
export function formatMicroUsd(microUsd: number): string {
  if (microUsd <= 0) return "free";
  const usd = microUsd / 1_000_000;
  if (usd >= 1) return `$${usd.toFixed(2)}`;
  if (usd >= 0.01) return `$${usd.toFixed(3)}`;
  return `$${usd.toFixed(5)}`;
}

/** Humanize a SCREAMING_SNAKE edge type into a readable relation phrase. */
export function humanizeEdge(edge: EdgeType): string {
  switch (edge) {
    case "PARENT_CHILD":
      return "parent";
    case "BRANCH":
      return "branch";
    case "DERIVED_FROM":
      return "derived from";
    case "VALIDATES":
      return "validates";
    case "CHECKS":
      return "checks";
    case "STRESSES":
      return "stresses";
    case "MERGE_PARENT":
      return "merge parent";
    default:
      return String(edge).toLowerCase();
  }
}

/** The effective-status token shown on a card/header (status folded with stale). */
export interface EffectiveStatus {
  /** The raw lifecycle, used as the `data-status` styling key. */
  status: Lifecycle;
  /** Whether the node is stale (amber ring + "stale" tag). */
  stale: boolean;
  /** A human label, e.g. "passed", "running". */
  label: string;
}

/** Fold a node's `(status, isStale)` into one effective-status token (§6.2). */
export function effectiveStatus(
  status: Lifecycle,
  isStale: boolean,
): EffectiveStatus {
  return { status, stale: isStale, label: status };
}
