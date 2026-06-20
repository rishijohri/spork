#!/usr/bin/env bash
# Spork F3-UI full no-display verification (DESIGN.md §14).
#
# Runs the four green-bar steps the Definition of Done enforces, with NO display:
#   (a) npm install        — frontend deps
#   (b) npm run build      — tsc strict + vite build
#   (c) npm test           — Vitest component/unit suite (Tauri mocked)
#   (d) cargo build + clippy of app/src-tauri (Tauri backend embedding the daemon)
#
# Prints a clear PASS/FAIL per step and a final summary. cargo is invoked with
# the Spork-conventional PATH prefix since cargo is off PATH in this environment.

set -uo pipefail

# Resolve the app/ dir (this script lives in app/scripts/).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TAURI_DIR="${APP_DIR}/src-tauri"

export PATH="$HOME/.cargo/bin:$PATH"

FAILED=0

step() {
  local name="$1"
  shift
  echo ""
  echo "=== ${name} ==="
  if "$@"; then
    echo "--- PASS: ${name}"
  else
    echo "--- FAIL: ${name}"
    FAILED=1
  fi
}

# (a) install
step "npm install" bash -c "cd '${APP_DIR}' && npm install"

# (b) frontend build (tsc strict + vite)
step "frontend build (tsc -b && vite build)" bash -c "cd '${APP_DIR}' && npm run build"

# (c) frontend tests (Vitest, Tauri mocked, jsdom)
step "frontend tests (vitest run)" bash -c "cd '${APP_DIR}' && npm test"

# (d) Tauri backend build + clippy (frontend dist exists from step b)
step "tauri backend build (cargo build)" bash -c "cd '${TAURI_DIR}' && cargo build"
step "tauri backend lint (cargo clippy -D warnings)" bash -c "cd '${TAURI_DIR}' && cargo clippy --all-targets -- -D warnings"
step "tauri backend tests (cargo test)" bash -c "cd '${TAURI_DIR}' && cargo test"

echo ""
if [ "${FAILED}" -eq 0 ]; then
  echo "============================================"
  echo " ALL CHECKS PASSED (no display required)"
  echo "============================================"
  exit 0
else
  echo "============================================"
  echo " SOME CHECKS FAILED"
  echo "============================================"
  exit 1
fi
