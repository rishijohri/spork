// Daemon-backed reads via TanStack Query (DESIGN.md §14.1, §14.4).
//
// `graph_view` is server state (owned by the daemon), so it is fetched and
// cached by React Query rather than held in the zustand UI store. The query's
// result seeds the store's live view-model; the op-log stream then folds deltas
// on top of it. A node diff/blob read is also a daemon read and modeled here.

import {
  useQuery,
  type UseQueryResult,
} from "@tanstack/react-query";
import { dispatch, graphView } from "../ipc/client";
import type { CommandResult, GraphView, Ulid } from "../ipc/types";

/** Query key for the whole-graph view-model snapshot. */
export const GRAPH_VIEW_KEY = ["graphView"] as const;

/** Fetch + cache the denormalized view-model snapshot. */
export function useGraphView(): UseQueryResult<GraphView, Error> {
  return useQuery({
    queryKey: GRAPH_VIEW_KEY,
    queryFn: graphView,
  });
}

/** Query key for a node's changed-path diff against a baseline. */
export function nodeDiffKey(nodeId: Ulid, against: Ulid | null) {
  return ["nodeDiff", nodeId, against] as const;
}

/**
 * Fetch a node's changed-path set lazily (DESIGN.md §14.5 — selection is
 * O(changed files)). Disabled until a node id is provided.
 */
export function useNodeDiff(
  nodeId: Ulid | null,
  against: Ulid | null = null,
): UseQueryResult<string[], Error> {
  return useQuery({
    queryKey: nodeDiffKey(nodeId ?? "", against),
    enabled: nodeId !== null,
    queryFn: async (): Promise<string[]> => {
      const res: CommandResult = await dispatch({
        command: "NODE_DIFF",
        nodeId: nodeId as Ulid,
        against,
      });
      return res.result === "DIFF" ? res.changedPaths : [];
    },
  });
}
