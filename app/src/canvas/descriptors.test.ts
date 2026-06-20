// Descriptor tests (DESIGN.md §14.5) — schema-driven node cards off the daemon's
// `ui_contributions`, with the six built-ins mirroring the frozen Rust values
// and an unknown kind degrading to a neutral fallback (forward-tolerant).

import { describe, it, expect } from "vitest";
import {
  BUILTIN_DESCRIPTORS,
  allDescriptors,
  descriptorFor,
  descriptorFromUiContributions,
  iconGlyph,
} from "./descriptors";

describe("descriptorFor", () => {
  it("resolves all six P5 built-ins with their frozen ui_contributions", () => {
    // Colors verified against crates/spork-nodes/src/*.rs `ui_contributions`.
    expect(descriptorFor("codebase-edit").color).toBe("#22c55e");
    expect(descriptorFor("codebase-edit").label).toBe("Edit");
    expect(descriptorFor("snapshot").color).toBe("#4f8cff");
    expect(descriptorFor("merge").color).toBe("#ec4899");
    expect(descriptorFor("sanity").color).toBe("#f59e0b");
    expect(descriptorFor("validation").color).toBe("#3b82f6");
    expect(descriptorFor("stress").color).toBe("#a855f7");
  });

  it("assigns the correct family to each built-in", () => {
    expect(descriptorFor("codebase-edit").family).toBe("mutating");
    expect(descriptorFor("snapshot").family).toBe("mutating");
    expect(descriptorFor("merge").family).toBe("mutating");
    expect(descriptorFor("validation").family).toBe("observing");
    expect(descriptorFor("sanity").family).toBe("observing");
    expect(descriptorFor("stress").family).toBe("observing");
  });

  it("falls back to a neutral descriptor for an unknown/custom kind", () => {
    const d = descriptorFor("my-custom-type");
    expect(d.kind).toBe("my-custom-type");
    expect(d.label).toBe("my-custom-type");
    expect(d.color).toBe("#9ca3af");
    expect(d.family).toBe("context");
  });

  it("exposes exactly the six built-ins in the legend", () => {
    expect(allDescriptors()).toHaveLength(6);
    expect(Object.keys(BUILTIN_DESCRIPTORS).sort()).toEqual(
      ["codebase-edit", "merge", "sanity", "snapshot", "stress", "validation"].sort(),
    );
  });
});

describe("descriptorFromUiContributions (schema-driven)", () => {
  it("parses a daemon ui_contributions object into a descriptor", () => {
    // A custom node type renders IDENTICALLY to built-ins from its descriptor.
    const d = descriptorFromUiContributions(
      "custom-fuzz",
      { color: "#abcdef", icon: "activity", displayName: "Fuzz" },
      "observing",
    );
    expect(d.kind).toBe("custom-fuzz");
    expect(d.color).toBe("#abcdef");
    expect(d.label).toBe("Fuzz");
    expect(d.icon).toBe("⚡"); // "activity" glyph
    expect(d.family).toBe("observing");
  });

  it("degrades gracefully on a malformed/empty contribution (no throw)", () => {
    const d = descriptorFromUiContributions("weird", {});
    expect(d.color).toBe("#9ca3af"); // neutral fallback color
    expect(d.label).toBe("weird");
    const dn = descriptorFromUiContributions("weird", null);
    expect(dn.color).toBe("#9ca3af");
  });

  it("ignores wrong-typed fields and keeps the fallback", () => {
    const d = descriptorFromUiContributions("weird", {
      color: 123,
      icon: false,
      displayName: ["x"],
    });
    expect(d.color).toBe("#9ca3af");
    expect(d.label).toBe("weird");
    expect(d.icon).toBe("○");
  });
});

describe("iconGlyph", () => {
  it("maps known icon names to glyphs and unknown to a neutral dot", () => {
    expect(iconGlyph("pencil")).toBe("✎");
    expect(iconGlyph("camera")).toBe("▣");
    expect(iconGlyph("totally-unknown")).toBe("○");
    expect(iconGlyph(undefined)).toBe("○");
  });
});
