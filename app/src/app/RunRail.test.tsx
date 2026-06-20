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

  it("shows an empty state for the activity log when there is no activity", () => {
    render(<RunRail />);
    expect(screen.getByText("No activity yet.")).toBeInTheDocument();
  });

  it("renders activity entries, with an error entry styled distinctly", () => {
    render(<RunRail />);

    act(() => {
      useUiStore.getState().logActivity("success", "Committed node → branch");
      useUiStore.getState().logActivity("error", "Push to GitHub failed: denied");
    });

    // The log is an accessible log region.
    const region = screen.getByRole("log", { name: "Activity log" });
    expect(region).toBeInTheDocument();

    // Both entries render, newest last.
    const entries = screen.getAllByTestId("activity-entry");
    expect(entries).toHaveLength(2);
    expect(entries[0]?.textContent).toContain("Committed node → branch");
    expect(entries[1]?.textContent).toContain("Push to GitHub failed: denied");

    // The error entry carries its distinct level marker + style class.
    expect(entries[1]).toHaveAttribute("data-level", "error");
    expect(entries[1]?.className).toContain("spork-activity-error");
    // The success entry is styled distinctly from the error one.
    expect(entries[0]).toHaveAttribute("data-level", "success");
    expect(entries[0]?.className).toContain("spork-activity-success");

    // The empty-state line is gone once there is activity.
    expect(screen.queryByText("No activity yet.")).not.toBeInTheDocument();
  });
});
