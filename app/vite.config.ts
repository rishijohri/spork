import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Spork F3-UI Vite + Vitest config (DESIGN.md §14.1, §14.4).
//
// Tauri expects the dev server on a fixed port and the production bundle in
// `dist/` (which the Rust backend embeds via `generate_context!`). The fixed
// 1420 port matches `src-tauri/tauri.conf.json`'s `build.devUrl`.
//
// The `test` block configures Vitest in-process (one Vite version, so no plugin
// type clash): jsdom, the Tauri-mock setup file, and the ResizeObserver
// polyfill needed by React Flow — the no-display verification bar.
const host = process.env["TAURI_DEV_HOST"];

export default defineConfig({
  plugins: [react()],

  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // The Rust backend has its own watcher; don't double-watch it.
      ignored: ["**/src-tauri/**"],
    },
  },

  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    sourcemap: false,
  },

  envPrefix: ["VITE_", "TAURI_"],

  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    css: false,
  },
});
