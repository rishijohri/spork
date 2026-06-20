// Vitest global setup (DESIGN.md §14.4–§14.5).
//
// Two things every test needs in jsdom, with no display and no Tauri host:
//   1. `ResizeObserver` — React Flow measures node DOM through it; jsdom has no
//      implementation, so we install a no-op polyfill.
//   2. The `@tauri-apps/api` surface — there is no Tauri runtime under Vitest,
//      so `invoke`/`listen` are replaced by the in-memory mock in
//      `src/ipc/mock.ts`. Tests drive that mock's fixtures/event replay.
import "@testing-library/jest-dom/vitest";
import { vi, beforeEach } from "vitest";
import { resetTauriMock, mockInvoke, mockListen } from "../ipc/mock";

// --- ResizeObserver polyfill (React Flow) ------------------------------------
class ResizeObserverMock {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
globalThis.ResizeObserver =
  ResizeObserverMock as unknown as typeof ResizeObserver;

// jsdom lacks these layout primitives React Flow reads; stub them to zero so the
// canvas mounts without throwing.
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}
if (typeof DOMMatrixReadOnly === "undefined") {
  // React Flow's transform math touches DOMMatrixReadOnly in some paths.
  // A minimal identity stand-in is enough for headless rendering.
  class DOMMatrixReadOnlyMock {
    m22 = 1;
    constructor(_t?: string) {}
  }
  (globalThis as unknown as { DOMMatrixReadOnly: unknown }).DOMMatrixReadOnly =
    DOMMatrixReadOnlyMock;
}

// --- Tauri IPC mock ----------------------------------------------------------
// Route every `@tauri-apps/api/core` invoke and `@tauri-apps/api/event` listen
// through the in-memory mock. The mock is reset before each test so fixtures and
// recorded calls never leak across tests.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mockListen,
}));

beforeEach(() => {
  resetTauriMock();
});
