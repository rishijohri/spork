// Formatter tests (UI_UX_DESIGN.md §3.3, §12.5) — the cost-ledger dollar format
// and the short-id/model helpers behind the P6 cost surfaces.

import { describe, it, expect } from "vitest";
import { formatMicroUsd, humanizeModel, shortId } from "./format";

describe("formatMicroUsd", () => {
  it("renders zero (and negatives) as 'free'", () => {
    expect(formatMicroUsd(0)).toBe("free");
    expect(formatMicroUsd(-5)).toBe("free");
  });

  it("scales micro-USD by 1e6 and picks precision by magnitude", () => {
    // 8100 micro-USD = $0.00810 — sub-cent → 5dp (the off-by-1000 guard).
    expect(formatMicroUsd(8_100)).toBe("$0.00810");
    // 12_345 micro = $0.012345 → >= 1¢ → 3dp.
    expect(formatMicroUsd(12_345)).toBe("$0.012");
    // 2_500_000 micro = $2.50 → >= $1 → 2dp.
    expect(formatMicroUsd(2_500_000)).toBe("$2.50");
  });
});

describe("humanizeModel / shortId", () => {
  it("drops the provider prefix from a model key", () => {
    expect(humanizeModel("anthropic/claude-sonnet-4-6")).toBe("claude-sonnet-4-6");
    expect(humanizeModel("llama3.1")).toBe("llama3.1");
    expect(humanizeModel(null)).toBe("—");
  });

  it("shortens a long ULID to its entropy tail", () => {
    expect(shortId("00000000000000000000000001")).toBe("…0000001");
    expect(shortId(null)).toBe("—");
  });
});
