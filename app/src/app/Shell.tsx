// The application shell (UI_UX_DESIGN.md §4).
//
// Five regions — top bar · left navigator · center canvas · right node-details ·
// bottom status/run rail (rail + status sub-strip) — plus the overlay host
// (modals, context menu, command palette) and the disconnected banner. The shell
// owns the live wiring: it seeds the store from a graph_view read, folds the
// op-log + ephemeral streams, tracks daemon connection, applies the density
// preference, and binds ⌘K.

import { useEffect, useRef, type JSX } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { TopBar } from "./TopBar";
import { Navigator } from "./Navigator";
import { Canvas } from "../canvas/Canvas";
import { HeroChat } from "./HeroChat";
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
import { lastProject, openAndImport, forgetProject } from "./onboarding";
import { refreshLocalModels } from "./models";

/** The composed shell. */
export function Shell(): JSX.Element {
  const setView = useUiStore((s) => s.setView);
  const ingestEvent = useUiStore((s) => s.ingestEvent);
  const ingestEphemeral = useUiStore((s) => s.ingestEphemeral);
  const setConnection = useUiStore((s) => s.setConnection);
  const setPaletteOpen = useUiStore((s) => s.setPaletteOpen);
  const navCollapsed = useUiStore((s) => s.navCollapsed);
  const detailsCollapsed = useUiStore((s) => s.detailsCollapsed);
  const navWidth = useUiStore((s) => s.navWidth);
  const detailsWidth = useUiStore((s) => s.detailsWidth);
  const centerView = useUiStore((s) => s.centerView);
  const density = useUiStore((s) => s.density);
  const connection = useUiStore((s) => s.connection);
  const projectOpening = useUiStore((s) => s.projectOpening);
  const setProjectOpening = useUiStore((s) => s.setProjectOpening);
  const logActivity = useUiStore((s) => s.logActivity);
  const qc = useQueryClient();
  const autoOpened = useRef(false);

  // Seed the live view-model from the daemon read snapshot.
  const graph = useGraphView();
  useEffect(() => {
    if (graph.data) setView(graph.data);
  }, [graph.data, setView]);

  // One-shot on launch (desktop only): auto-reopen the last project so a returning
  // user lands back in their graph instead of the onboarding picker (P7.5 MVP).
  // `projectOpening` gates the canvas + banner so the empty state never flashes.
  useEffect(() => {
    if (autoOpened.current || !isTauri()) return;
    autoOpened.current = true;
    const path = lastProject();
    if (!path) return;
    setProjectOpening(true);
    void (async () => {
      try {
        await openAndImport(path, qc);
        logActivity("success", `Reopened ${path}`);
      } catch (err) {
        // A stale / unreadable project — forget it and fall back to the picker.
        forgetProject();
        logActivity(
          "error",
          `Could not reopen ${path}: ${err instanceof Error ? err.message : String(err)}`,
        );
      } finally {
        setProjectOpening(false);
      }
    })();
  }, [qc, setProjectOpening, logActivity]);

  // Connection state: browser (no Tauri) = mock; a "no project open" read error
  // is first-run onboarding (the daemon is alive, it just has no project) — NOT a
  // disconnection; any other read error = disconnected.
  useEffect(() => {
    if (!isTauri()) setConnection("mock");
    else if (graph.isError) {
      setConnection(isNoProjectError(graph.error) ? "no-project" : "disconnected");
    } else if (graph.data) setConnection("connected");
  }, [graph.isError, graph.error, graph.data, setConnection]);

  // Apply the density preference to the document element.
  useEffect(() => {
    document.documentElement.dataset.density = density;
  }, [density]);

  // Probe the configured (or default) local server for its REAL installed models
  // (no-stub honesty) whenever the provider changes — the selector then offers
  // only what's actually reachable, never a hardcoded guess.
  const agentProvider = useUiStore((s) => s.agentProvider);
  useEffect(() => {
    void refreshLocalModels();
  }, [agentProvider]);

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
      {connection === "no-project" && !projectOpening && (
        <div className="spork-banner spork-banner--info" role="status">
          <Icon name="folder" size={15} />
          <span>No project open — open one to begin (the canvas below has the picker).</span>
        </div>
      )}
      <div
        className="spork-main"
        data-nav={navCollapsed ? "collapsed" : "expanded"}
        data-details={detailsCollapsed ? "collapsed" : "expanded"}
        style={{
          ["--nav-w" as string]: `${navWidth}px`,
          ["--details-w" as string]: `${detailsWidth}px`,
        }}
      >
        <Navigator />
        <main className="spork-canvas-region" style={{ minWidth: 0, minHeight: 0 }}>
          {centerView === "chat" ? <HeroChat /> : <Canvas />}
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

/**
 * Whether a `graph_view` read error is the benign first-run "no project open"
 * condition (the daemon is alive but no project has been opened) rather than a
 * real disconnection. The Tauri backend rejects with the string "no project
 * open" until `open_project` runs; the rejection reaches us as a string or an
 * Error, so we stringify defensively.
 */
export function isNoProjectError(error: unknown): boolean {
  const text =
    error instanceof Error ? error.message : typeof error === "string" ? error : String(error);
  return /no project open/i.test(text);
}
