// Run-rail test (UI_UX_DESIGN.md §5.9, §7.5) — the v1 rail is the Activity
// stream; the "Run output" tab is a disabled forward-map placeholder.

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { RunRail } from "./RunRail";
import { useUiStore } from "../state/store";

describe("RunRail", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("shows the empty state when there is no activity", () => {
    render(<RunRail />);
    expect(screen.getByText("No activity yet.")).toBeInTheDocument();
    expect(screen.queryByTestId("activity-log")).not.toBeInTheDocument();
  });

  it("renders a success activity entry inside the activity log", () => {
    render(<RunRail />);

    act(() => {
      useUiStore.getState().logActivity("success", "Committed X");
    });

    const log = screen.getByTestId("activity-log");
    const entries = screen.getAllByTestId("activity-entry");
    expect(entries).toHaveLength(1);

    const entry = entries[0]!;
    expect(log).toContainElement(entry);
    expect(entry).toHaveAttribute("data-level", "success");
    expect(entry.textContent).toContain("Committed X");

    // The empty state is gone once there is activity.
    expect(screen.queryByText("No activity yet.")).not.toBeInTheDocument();
  });

  it("renders an error-level activity entry distinctly", () => {
    render(<RunRail />);

    act(() => {
      useUiStore.getState().logActivity("error", "Push failed: denied");
    });

    const entry = screen.getByTestId("activity-entry");
    expect(entry).toHaveAttribute("data-level", "error");
    expect(entry.textContent).toContain("Push failed: denied");
  });

  it("clears the activity log via the Clear control", () => {
    render(<RunRail />);

    act(() => {
      useUiStore.getState().logActivity("info", "Started run");
      useUiStore.getState().logActivity("success", "Run passed");
    });
    expect(screen.getAllByTestId("activity-entry")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Clear activity" }));

    expect(screen.queryByTestId("activity-entry")).not.toBeInTheDocument();
    expect(screen.queryByTestId("activity-log")).not.toBeInTheDocument();
    expect(screen.getByText("No activity yet.")).toBeInTheDocument();
  });

  it("shows the Run output tab as present but disabled", () => {
    render(<RunRail />);

    const tab = screen.getByRole("tab", { name: "Run output" });
    expect(tab).toBeInTheDocument();
    expect(tab).toBeDisabled();
    expect(tab).toHaveAttribute("aria-selected", "false");

    // The Activity tab is the active one.
    const activityTab = screen.getByRole("tab", { name: "Activity" });
    expect(activityTab).toHaveAttribute("aria-selected", "true");
  });
});
