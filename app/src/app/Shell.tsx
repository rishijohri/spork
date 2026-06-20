// The five-region application shell (DESIGN.md §14.2).
//
// TOP BAR (model selector + toolbar) · LEFT navigator + legend · CENTER DAG
// canvas · RIGHT Node-Details panel · BOTTOM status/run rail. The shell also
// owns the live wiring: it subscribes to the op-log + ephemeral channels and
// folds them into the store, and seeds the store from a `graph_view` fetch.

import { useEffect } from "react";
import { TopBar } from "./TopBar";
import { Legend } from "./Legend";
import { Canvas } from "../canvas/Canvas";
import { NodeDetails } from "./NodeDetails";
import { RunRail } from "./RunRail";
import { useUiStore } from "../state/store";
import { useGraphView } from "../state/queries";
import { listenOpLog, listenEphemeral, type Unlisten } from "../ipc/client";

/** The composed shell. */
export function Shell(): JSX.Element {
  const setView = useUiStore((s) => s.setView);
  const ingestEvent = useUiStore((s) => s.ingestEvent);
  const ingestEphemeral = useUiStore((s) => s.ingestEphemeral);

  // Seed the live view-model from the daemon read snapshot.
  const graph = useGraphView();
  useEffect(() => {
    if (graph.data) setView(graph.data);
  }, [graph.data, setView]);

  // Subscribe to the ordered op-log + the ephemeral side-channels; fold each.
  useEffect(() => {
    let unsubOpLog: Unlisten | undefined;
    let unsubEph: Unlisten | undefined;
    let active = true;

    void listenOpLog((e) => ingestEvent(e)).then((u) => {
      if (active) unsubOpLog = u;
      else u();
    });
    void listenEphemeral((f) => ingestEphemeral(f)).then((u) => {
      if (active) unsubEph = u;
      else u();
    });

    return () => {
      active = false;
      unsubOpLog?.();
      unsubEph?.();
    };
  }, [ingestEvent, ingestEphemeral]);

  return (
    <div className="spork-shell">
      <TopBar />
      <div className="spork-shell-main">
        <Legend />
        <main className="spork-shell-canvas">
          <Canvas />
        </main>
        <NodeDetails />
      </div>
      <RunRail />
    </div>
  );
}
