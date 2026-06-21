# Spork — Project Instructions for Claude

> Read this first. It orients you to the project, the rules you must follow, and where the detail lives. This file is intentionally short; the docs in `docs/` are authoritative.

## What Spork is
An **agent-centric IDE** built around a persistent, project-level **branching timeline (a DAG) of typed work nodes**, where every node carries a **content-addressed, restorable sandbox** of the codebase. Local-first, multi-provider, lineage-aware context. The defining bet (D1+D2+D3): a *typed work-DAG with per-node restorable sandboxes as the primary surface* — which a prior-art sweep found is genuinely unbuilt today (see `docs/MARKET_STUDY.md`).

The six dimensions: **D1** project-level branching work-DAG · **D2** typed node types (edit / validation / stress / deterministic auto-running sanity, + user-definable) · **D3** per-node content-addressed restorable sandbox · **D4** local-first, expandable to shared-team via sandbox-state sharing · **D5** multi-provider with fast switching · **D6** lineage-aware managed context + handoff docs.

## Repo status
**Foundation COMPLETE (F0–F4) · P5 done · F3-UI redesigned · P6 done · P7 done; P8–P9 are next (purely additive).** A Rust cargo workspace under `crates/` built phase by phase per `docs/TODO.md`. Current: **35 crates, 1145 workspace tests + 81 app (Vitest) tests green**, clippy `-D warnings` + rustfmt clean, tsc + vite build + Tauri `cargo check` green, no stub macros; on branch `feat/f3-ui-redesign` (PR #1). P7 added quality gates + baselines + flaky handling (`spork-gates`/`spork-baseline`), the full constraint scheduler (`ConstraintScheduler` behind the frozen F4 `Scheduler` trait), the lineage-aware Context Compiler (`LineageCompiler` behind the frozen `ContextCompiler` seam) + per-node-type `ContextPolicy` defaults + repo map (`spork-repomap`) + memory store (`spork-memory`), and the read-only Lineage/History MCP (`spork-history`) — plus the daemon wiring (`BranchMergeGated` gates post-merge re-run results and the immutable `GateVerdict` travels with the snapshot; `NodeCheckout` applies §6.6 fork-on-divergence; `NodeContext`/`NodeHandoff`/`HistoryQuery` reads) and the paired UI activation (gate badge, context/handoff/history affordances, gated-merge toggle, checkout action) — all behind frozen seams. The P7 freezes (the `GatePolicy` predicate grammar, `GateVerdict`-travels-with-snapshot, sibling-isolation Q4) are locked for P9. Every load-bearing contract is frozen with exactly one+ real impl, so P8–P9 add implementations behind existing seams — never edit a frozen contract (see plan §9 D-1…D-15 first). `docs/TODO.md` is the live source of progress — read it for the exact next step.

- F0 `spork-hash`/`-canon`/`-ignore`/`-cas`/`-asset`/`-cas-cli`: content-addressed store (BLAKE3 + FastCDC), canonical identity, ignore profile, asset store.
- F1 `spork-log`/`-projection`/`-migrate`: append-only hash-chained event log + single writer actor + rebuildable projection + migration registry.
- F2 `spork-status`/`-edges`/`-registry`/`-graph`: NodeEnvelope, NodeTypeRegistry, typed acyclic edges, effective_status — the graph is a pure projection of the F1 log.
- F3 (headless) `spork-ipc`/`-broker`/`-vault`/`-stream`/`-drift`/`-restore`/`-git`/`-daemon`: typed IPC (opId/events), capability broker, credential vault, dual-channel stream, three-source drift capture, atomic dual-restore, non-invasive git — driven by a headless client.
- F4 `spork-exec`/`-runner`/`-provider`/`-context`/`-merge`: worktree-on-CoW execution + scheduler + lease/reaper + EnvManifest; Runner SPI + one ResultEnvelope + content-addressed cache; provider port + canonical-transcript mapping (offline AnthropicAdapter); cache-aligned context compiler + handoff; merge schemas.

**Deferred — F3-UI (Tauri/React DAG canvas):** split out (TODO §F3-UI, plan §11.5). The backend is now through F4, so the remaining gate is a **GUI-capable environment**; recommended pick-up is right after P5. It binds to the F3-frozen IPC/view-model contracts, so it is additive — do not block P-phases on it, and do not attempt it headlessly here.

## The documents (in `docs/`)
| File | Role |
|---|---|
| [DESIGN.md](docs/DESIGN.md) | **Authoritative spec.** 19 sections + appendix. Cite its section numbers (e.g. "§10.3") in code and PRs. |
| [IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) | The no-stub / no-domino build plan: foundational primitives & schemas (§4), extension-point catalog (§5), the F0–P9 build spine (§6), cross-cutting concerns (§7), DoD discipline (§8), the **Domino-Risk Register** (§9, D-1…D-15), edge cases (§12). |
| [TODO.md](docs/TODO.md) | Phase-by-phase task tracker derived from the plan. **Keep it updated as the single source of progress.** |
| [IDEA.md](docs/IDEA.md) | Short, durable capture of the product vision (the six dimensions, the bet, the failure modes it fixes). Read it to stay oriented; it is stable. |
| [UI_UX_DESIGN.md](docs/UI_UX_DESIGN.md) | **Authoritative UI/UX spec — a *living* document covering ALL phases.** Wireframes + every component mapped to its visual change and backing command, with a feature↔plan honesty matrix. Surfaces are tagged 🟢 *implemented now* vs 🟡 *designed now, built when its phase (P6–P9) lands*. See its §16 phase-completion ritual. |
| [MARKET_STUDY.md](docs/MARKET_STUDY.md) | Why this is worth building (prior-art + adoption verdict). Context, not a build input. |

## Non-negotiable engineering constraints
These govern every change. They are the whole point of the plan; do not relax them.

- **C1 — No stubs.** Ship *narrow-but-complete* slices, never a hollow shell with the hard part deferred. One real implementation behind every seam. A CI gate (plan §8.3) forbids `todo!()`/`unimplemented!()`/`TODO`/`FIXME` in `src/**`. A genuinely deferred item is an explicit, documented out-of-scope — never a silent placeholder.
- **C2 — No domino.** Never change a frozen contract in place. Grow it via a new implementation behind its seam, or a new **versioned generation**. The load-bearing decisions (D-1…D-15 in plan §9) are frozen in F0–F4 *before* anything depends on them. **If you think you need to change a frozen foundation contract, STOP and re-read plan §9 first** — it almost certainly has an additive escape hatch.
- **C3 — Extensible & additive.** Node types, providers, sandbox tiers, transports, storage backends, check runners, context strategies, asset/dependency providers are all plugin-shaped from day one (plan §5). Adding one is a new impl behind an existing interface, never a core edit. Built-ins register through the *same* public registries third parties use.
- **C4 — Design-consistent.** Every primitive/schema traces to a `DESIGN.md` section and may refine but never contradict it.
- **C5 — Evolution-safe.** Every persisted struct carries a `schema_version` + a registered forward-migration from its first commit. Migrations apply at replay/checkpoint; stored events are never edited.

## The build spine (strictly linear, non-reorderable)
`F0 → F1 → F2 → F3 → F4 → P5 → P6 → P7 → P8 → P9`. A phase starts only when the previous phase's DoD is fully met and its freeze-before contracts are locked.

- **F0 Byte-Identity Substrate** — BLAKE3 (generation-tagged) CAS, canonical serialization, FastCDC, `ignore_profile_hash`, `StorageBackend` + `AssetStore` traits.
- **F1 Event-Sourcing Core** — append-only hash-chained log behind one writer actor; projection = pure fn of the log; migration registry.
- **F2 Typed-Graph Contracts as Data** — `NodeEnvelope`, `NodeTypeRegistry`/`NodeTypeDescriptor`, typed acyclic edges, `effectiveStatus`.
- **F3 Daemon/Renderer Seam + Interactive Core** — typed IPC (`opId`+events), capability broker, vault, three-source drift capture, atomic dual-restore, DAG canvas. **Retires the untracked-mutation gap and the chat-only-restore trap here.**
- **F4 Execution/Result/Provider/Context Seams** — `IsolationBackend`, `Runner` SPI + `ResultEnvelope`, `ProviderAdapter` + canonical transcript, `ContextLayer` ranks, `EnvManifest`, v1 `AssetStore` — one impl each.
- **P5** built-in node types → **P6** multi-provider routing + cost → **P7** gates/scheduler/context compiler + Lineage/History MCP → **P8** SDK/tiered executors/marketplace → **P9** shared-team bundle grafting.

## Tech stack
**Daemon:** Rust — owns the DAG, BLAKE3 CAS (loose objects + packfiles), SQLite-WAL event log + projection, FastCDC, worktrees, routing, execution. **Shell/UI:** Tauri + React 18/TS, React Flow + ELK (Web Worker), Zustand + TanStack Query, Monaco diff. **Isolation:** WASM (WASI P2) → OCI container → microVM (Firecracker via Lima/Colima on macOS). **Model access:** Vercel AI SDK in a daemon-supervised Node.js sidecar + LiteLLM-style proxy / native Rust clients. **IPC:** tRPC-style typed commands + op-log event stream. **Context:** tree-sitter repo map + LSP/Serena via MCP.

## How to work in this repo
1. **Before implementing any component, read its `DESIGN.md` section(s)** and its `TODO.md` phase entry. Match the spec; cite section numbers.
2. **Stay in the current phase.** Don't pull later-phase work forward; don't leave earlier-phase stubs behind. Work the `TODO.md` checkboxes in order and update them.
3. **New capability ⇒ new impl behind an existing seam** (plan §5). If you can't add it without editing core/engine code, the seam is wrong — flag it, don't hack around it.
4. **Touching anything persisted ⇒ add `schema_version` + a migration.** Touching anything hashed ⇒ update golden vectors + bump the generation tag, never rehash in place.
5. **Secrets** are referenced only by `vaultRef`, resolved only inside the daemon, never written into the CAS (snapshots are exportable). The renderer holds zero secrets.
6. **Verify before you claim green.** cargo is off PATH — prefix with `export PATH="$HOME/.cargo/bin:$PATH"`. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` yourself from the repo root. **Do not trust a subagent's self-report** — they go stale (e.g. an agent adds tests after its last `fmt` run and reports `fmt clean`, but `--check` fails).
7. **At the end of every phase (required ritual):** check off that phase's Build + DoD boxes in `docs/TODO.md`, append a `✅ verified (N tests green)` note, update the **Current status** line, and commit the docs as `docs: mark Fx done …` alongside the code commit `Fx: <name> …`. A box becomes `[x]` only after you independently re-verified it green. This keeps `TODO.md` the trustworthy single source of progress for the next session.
8. **Core change ⇒ UI/UX change (paired, never separate).** Any change to user-facing functionality — a new IPC command, a new view-model field, a newly-*produced* event/stream/result, a new capability — must be paired with a matching update to `docs/UI_UX_DESIGN.md` *and then* the corresponding UI work. The doc already designs every future phase (its §14); when a phase completes, run its **§16 phase-completion ritual**: re-tag the unlocked surfaces 🟡→🟢, move them from §14 into the main body, re-check the honesty matrix (§13), then build them in `app/`. If a backend change has **no** UI consequence, say so explicitly ("no UI surface") so the omission is a decision, not an oversight. The UI/UX doc must never lag the daemon, and the UI must never advertise a capability the daemon can't fulfil.

## Key invariants & gotchas (so you don't re-derive them)
- **Content-addressing is identity.** Unchanged files cost zero new bytes (hash-pointer reuse); changed large files re-store only changed FastCDC chunks. It is dedup-by-hash, **not** byte-diff/patch storage.
- **The event log is the single source of truth.** The node/edge graph is a pure projection — droppable and rebuildable bit-for-bit. The writer actor is the only write path.
- **Dependencies (node_modules/venv/model files) are excluded from snapshots** and reconstructed read-only from the content-addressed, **platform-keyed**, offline-safe `AssetStore` (DESIGN §10.5). The lockfile is what's snapshotted. This keeps many sandboxes cheap; the deps-excluded decision is frozen in F0 (it affects `ignore_profile_hash`).
- **Restore scope is code + conversation only** (transactional, fail-closed). External side effects (shared-DB writes, pushed remotes, paid-API spend) are *not* undoable — surfaced via the declared effects-log seam, not silently (DESIGN §11.4, §12 of the plan).
- **Restore is non-destructive** — it's an event, not an overwrite; forward history survives as a branch.
- **Branching is automatic & daemon-owned (fork-on-divergence)** — code-changing work from a branch *tip* continues it; work from a *non-tip* node auto-forks a new branch, so a line is never silently overwritten. Read-only work (analysis/planning) *attaches* as an observing/context node (no snapshot, dotted edge), not a branch. The policy lives in the daemon's create-node/drift-reconcile/agent-run paths (additive over the caller-supplied `branch_id`, not a contract change), NOT in the renderer. Triggers: agent-run = P6, historical checkout = P7 (DESIGN §6.6). Lineage edges render solid, attachment/observation edges dotted.
- **Worktrees materialize via CoW** (reflink/clonefile) so checkout is O(changed bytes); inspection (click a node) is O(changed files) via lazy CAS diff — no worktree needed just to look.

## Conventions
- Crates under `crates/`, binaries under `bin/`, the Tauri/React app under `app/` (see `TODO.md` per-phase component paths).
- End git commit messages with the Co-Authored-By trailer for Claude. Commit/branch only when asked; if on `main`, branch first.
