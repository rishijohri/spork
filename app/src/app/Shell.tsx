// The application shell (UI_UX_DESIGN.md §4).
//
// Five regions — top bar · left navigator · center canvas · right node-details ·
// bottom status/run rail (rail + status sub-strip) — plus the overlay host
// (modals, context menu, command palette) and the disconnected banner. The shell
// owns the live wiring: it seeds the store from a graph_view read, folds the
// op-log + ephemeral streams, tracks daemon connection, applies the density
// preference, and binds ⌘K.

import { useEffect, type JSX } from "react";
import { TopBar } from "./TopBar";
import { Navigator } from "./Navigator";
import { Canvas } from "../canvas/Canvas";
import { NodeDetails } from "./NodeDetails";
import { RunRail } from "./RunRail";
import { StatusStrip } from "./StatusStrip";
import { Modals } from "./overlays/Modals";
import { ContextMenu } from "./overlays/ContextMenu";
import { CommandPalette } from "./overlays/CommandPalette";
import { Icon } from "../ui/icons";
import { useUiStore } from "../state/store";
import { useGraphView } from "../state/queries";
import { listenOpLog, listenEphemeral, isTauri, type Unlisten } from "../ipc/client";

/** The composed shell. */
export function Shell(): JSX.Element {
  const setView = useUiStore((s) => s.setView);
  const ingestEvent = useUiStore((s) => s.ingestEvent);
  const ingestEphemeral = useUiStore((s) => s.ingestEphemeral);
  const setConnection = useUiStore((s) => s.setConnection);
  const setPaletteOpen = useUiStore((s) => s.setPaletteOpen);
  const navCollapsed = useUiStore((s) => s.navCollapsed);
  const detailsCollapsed = useUiStore((s) => s.detailsCollapsed);
  const density = useUiStore((s) => s.density);
  const connection = useUiStore((s) => s.connection);

  // Seed the live view-model from the daemon read snapshot.
  const graph = useGraphView();
  useEffect(() => {
    if (graph.data) setView(graph.data);
  }, [graph.data, setView]);

  // Connection state: browser (no Tauri) = mock; a read error = disconnected.
  useEffect(() => {
    if (!isTauri()) setConnection("mock");
    else if (graph.isError) setConnection("disconnected");
    else if (graph.data) setConnection("connected");
  }, [graph.isError, graph.data, setConnection]);

  // Apply the density preference to the document element.
  useEffect(() => {
    document.documentElement.dataset.density = density;
  }, [density]);

  // Subscribe to the ordered op-log + ephemeral side-channels; fold each.
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

  // ⌘K / Ctrl+K opens the command palette.
  useEffect(() => {
    function onKey(e: KeyboardEvent): void {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen(true);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setPaletteOpen]);

  return (
    <div className="spork-shell">
      <TopBar />
      {connection === "disconnected" && (
        <div className="spork-banner" role="alert">
          <Icon name="alert-triangle" size={15} />
          <span>Lost connection to the Spork daemon.</span>
          <span className="spork-banner-spacer" />
          <button
            className="btn btn--sm"
            onClick={() => {
              setConnection("reconnecting");
              void graph.refetch();
            }}
          >
            Retry now
          </button>
        </div>
      )}
      <div
        className="spork-main"
        data-nav={navCollapsed ? "collapsed" : "expanded"}
        data-details={detailsCollapsed ? "collapsed" : "expanded"}
      >
        <Navigator />
        <main className="spork-canvas-region" style={{ minWidth: 0, minHeight: 0 }}>
          <Canvas />
        </main>
        {!detailsCollapsed && <NodeDetails />}
      </div>
      <RunRail />
      <StatusStrip
        onRetry={() => {
          setConnection("reconnecting");
          void graph.refetch();
        }}
      />

      {/* overlay host */}
      <Modals />
      <ContextMenu />
      <CommandPalette />
    </div>
  );
}
