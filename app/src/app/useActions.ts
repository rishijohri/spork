// The shared action runner (UI_UX_DESIGN.md §14.4, §7).
//
// One place that turns a frozen `Command` into: a dispatch, an optimistic op
// (for a MUTATION, so the op-log stream reconciles it), a human-readable activity
// line, and honest error surfacing — including routing a capability denial to the
// capability modal (§5.10e) instead of a bare error line. Every action surface
// (toolbar modals, run-check menu, context menu, command palette) goes through
// `run`, so dispatch/feedback live in exactly one place.

import { useCallback } from "react";
import { dispatch } from "../ipc/client";
import { useUiStore } from "../state/store";
import type { Command, CommandResult, Ulid } from "../ipc/types";
import {
  activityLineFor,
  capabilityForCommand,
  isCapabilityDenial,
  mintedSubjectId,
} from "./toolbar";

export interface RunOptions {
  /** The node the action concerns, for the activity line's short id. */
  nodeId?: Ulid | null;
  /** A label for the optimistic op + the capability modal's "action" field. */
  label?: string;
}

/** Hook returning `run(command, opts)` — the single dispatch+feedback path. */
export function useActions(): {
  run: (command: Command, opts?: RunOptions) => Promise<CommandResult | null>;
} {
  const beginOptimistic = useUiStore((s) => s.beginOptimistic);
  const logActivity = useUiStore((s) => s.logActivity);
  const openModal = useUiStore((s) => s.openModal);

  const run = useCallback(
    async (
      command: Command,
      opts: RunOptions = {},
    ): Promise<CommandResult | null> => {
      try {
        const res = await dispatch(command);
        const line = activityLineFor(command, res, opts.nodeId ?? null);
        if (line) logActivity(line.level, line.text);
        if (res.result === "MUTATION") {
          beginOptimistic(
            res.opId,
            opts.label ?? command.command,
            mintedSubjectId(res.ids),
          );
        }
        return res;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (isCapabilityDenial(message)) {
          // Surface the denial honestly (§5.10e) rather than a bare error line.
          openModal({
            kind: "capability",
            capability: capabilityForCommand(command),
            action: opts.label ?? command.command,
          });
        } else {
          logActivity("error", `${opts.label ?? "Action"} failed: ${message}`);
        }
        return null;
      }
    },
    [beginOptimistic, logActivity, openModal],
  );

  return { run };
}
