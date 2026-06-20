// Run-rail test (DESIGN.md §14.2, §14.4) — ephemeral frames stream in for the
// selected node without blocking the ordered path.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, act } from "@testing-library/react";
import { RunRail } from "./RunRail";
import { useUiStore } from "../state/store";

const NODE = "00000000000000000000000001";

describe("RunRail", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("streams ephemeral frames for the selected node into the rail", () => {
    useUiStore.getState().selectNode(NODE);
    render(<RunRail />);

    act(() => {
      useUiStore
        .getState()
        .ingestEphemeral({ nodeId: NODE, channel: "RUN_STDOUT", data: "running…\n" });
      useUiStore
        .getState()
        .ingestEphemeral({ nodeId: NODE, channel: "RUN_STDOUT", data: "ok\n" });
    });

    const log = screen.getByTestId("runrail-log");
    expect(log.textContent).toBe("running…\nok\n");
  });

  it("shows nothing for an unselected node", () => {
    render(<RunRail />);
    act(() => {
      useUiStore
        .getState()
        .ingestEphemeral({ nodeId: NODE, channel: "RUN_STDOUT", data: "x\n" });
    });
    const log = screen.getByTestId("runrail-log");
    expect(log.textContent).toBe("");
  });
});
