// First-run connection-state classification (the "no project open" fix).
//
// The Tauri backend rejects graph_view with "no project open" until a project is
// opened. The shell must treat that as benign onboarding (→ "no-project"), NOT
// an alarming "Lost connection" (→ "disconnected"). This guards the classifier
// that decides between the two regardless of whether the rejection arrives as a
// string or an Error.

import { describe, it, expect } from "vitest";
import { isNoProjectError } from "./Shell";

describe("isNoProjectError", () => {
  it("matches the backend 'no project open' rejection (string and Error)", () => {
    expect(isNoProjectError("no project open")).toBe(true);
    expect(isNoProjectError(new Error("no project open"))).toBe(true);
    // Tauri sometimes wraps the command error in a longer string.
    expect(isNoProjectError("invoke failed: no project open")).toBe(true);
  });

  it("does NOT match a real disconnection / other errors", () => {
    expect(isNoProjectError("daemon thread stopped")).toBe(false);
    expect(isNoProjectError(new Error("capability denied: model.invoke"))).toBe(false);
    expect(isNoProjectError(undefined)).toBe(false);
    expect(isNoProjectError(null)).toBe(false);
  });
});
