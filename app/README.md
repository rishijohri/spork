# Spork F3-UI (`app/`)

The Spork desktop IDE shell: a **Tauri v2** backend embedding the frozen F3
`spork-daemon`, with a **React 18 + TypeScript (strict)** renderer whose primary
surface is a left-to-right typed-node **DAG canvas** over the frozen daemon IPC
(see `docs/DESIGN.md` §14).

This is a **separate, standalone project** from the 27-crate core workspace. Its
Rust backend (`src-tauri/`) is its own cargo workspace and is `exclude`d from the
root `Cargo.toml`, so the UI can never break the core green-bar (CLAUDE.md
C2/C3).

## Architecture

- **Backend** (`src-tauri/`, Rust, Tauri v2): embeds `spork-daemon` on a
  dedicated owner thread (the daemon is single-threaded, so it never crosses a
  thread boundary) and exposes three `#[tauri::command]`s — `open_project`,
  `dispatch` (the single mutation/read path), `graph_view` (the view-model read
  snapshot) — plus a `ping` health check. Daemon op-log events are forwarded to
  the window as `oplog-event`; node-keyed ephemeral frames as `ephemeral`.
- **Frontend** (`src/`, React/TS): a typed IPC client (`src/ipc/`), a zustand
  store with a pure op-log reducer (`src/state/`), a React Flow + ELK DAG canvas
  (`src/canvas/`), and the five-region shell (`src/app/`): top bar (model
  selector + toolbar), left navigator + legend, center canvas, right
  Node-Details (chat / Monaco diff / results), bottom run rail.

The renderer holds zero secrets and never touches the filesystem or providers
directly — every privileged operation goes through the daemon (DESIGN.md §15.1).

## Prerequisites

- Node (tested on v26) + npm (v11)
- Rust (1.96). `cargo` may be off `PATH`; prefix with
  `export PATH="$HOME/.cargo/bin:$PATH"`.
- A WebKit-capable environment for the actual window (macOS ships it). **The
  window only runs with a display.**

## Install

```bash
cd app
npm install
```

## Develop

Run the full desktop app (Vite dev server + Tauri window). **Needs a display.**

```bash
cd app
npm run tauri:dev
```

Run only the frontend in a browser (no daemon, no window):

```bash
cd app
npm run dev      # http://localhost:1420
```

## Build

```bash
cd app
npm run build        # tsc strict + vite build -> dist/
npm run tauri:build  # full desktop bundle (embeds dist/; needs a display toolchain)
```

## Test (no display required)

```bash
cd app
npm test                      # Vitest component/unit suite, Tauri mocked (jsdom)
cd src-tauri && cargo build   # Tauri backend embedding the daemon
cd src-tauri && cargo test    # backend unit tests (real daemon over a temp dir)
```

Or run the whole no-display verification in one shot:

```bash
cd app
./scripts/check.sh            # install + build + test + cargo build/clippy/test
```

## Verification bar (Definition of Done — no display)

1. `cargo build --workspace` (the 27 core crates) still green (app excluded).
2. `cd app && npm install && npm run build` — tsc strict + vite build green.
3. `cd app && npm test` — Vitest suite green, Tauri commands mocked.
4. `cd app/src-tauri && cargo build` — backend embedding the daemon green
   (frontend `dist/` built first so `generate_context!` succeeds); clippy clean.

## VS Code

`.vscode/launch.json` provides **Tauri Development Debug** (and a production
variant); `.vscode/tasks.json` provides `ui:build`, `ui:test`, `ui:tauri-dev`,
and `ui:check`. Launch configs build the frontend first via a `preLaunchTask`.
