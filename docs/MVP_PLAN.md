# Spork — MVP Plan (P7.5 · Local Trusted-Agent MVP)

> **Status:** **IMPLEMENTED & verified** ✅ — both slices landed. **Slice A** (W1 import · W2 root-at-real-dir · W3 provider config · W5a transcript-read) and **Slice B** (W4 trusted edit loop · W5b conversation surface). Decisions taken at build time: edit-loop tools = read/write/list/run-command/**apply_patch**; ceilings = **20 iterations / $5.00**; **single-agent** edits (no fan-out helper). Spork is now a usable MVP. This is a standalone plan for the milestone; it is **separate from** [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) (which carries a short **P7.5** pointer here). See [TODO.md](TODO.md) §P7.5 for the live checklist.
>
> **Authoritative specs it operationalizes:** [DESIGN.md](DESIGN.md) §17 ("Phase 1 — Local MVP, the wedge"), and the UI surfaces in [UI_UX_DESIGN.md](UI_UX_DESIGN.md). Cite those section numbers in code/PRs; this doc adds **no new contract** — it is wiring over already-frozen F0–F4 seams.

---

## 0. TL;DR

By the end of P7 the **backend is ~7/10 phases complete**, but the **product is not a minimum viable product**: opening a real repo yields a *permanently empty canvas* because no IPC command, no UI affordance, and no daemon driver mints the first node. Every wired capability (run-check, branch, merge, restore, agent-ask, gated-merge, history) operates on a *pre-existing* node, so the missing first node blocks everything.

This plan closes that with **five wiring workstreams over shipped F4 seams** — **no new infrastructure, no frozen-contract edits** (all additive: new `Command` variants behind `#[non_exhaustive]`/append-only enums, new Tauri commands, new UI affordances). When done, a single user can: **open a real repo → see their code as a root node → configure a local/CLI agent → ask the agent to make a change → get a new Edit node (diff + transcript + auto-Sanity) that auto-forks on divergence → branch/merge/restore it → reopen the project intact.**

The only items deferred to P8 / post-MVP are genuinely new infrastructure: **untrusted-executor isolation (real P8)**, **cloud TLS transport**, and **live token streaming** — none of which a local-first wedge needs.

---

## 1. Why this exists — the gap the contract-first plan left

The build spine `F0→…→P9` is optimized for **contract-freezing** (the "no-domino" discipline, IMPLEMENTATION_PLAN §9). That discipline is sound, but it **shattered the one place an MVP was ever defined** — DESIGN.md §17 "Phase 1 — Local MVP" — across the F/P phases, with **no single phase owning the user journey whole**. Every phase Definition of Done asserts an *infrastructure contract* ("a merge is blocked by a post-merge gate"), never *"a user can take a fresh repo to first useful work."* So "1145+ tests, all core built" measures **frozen-contract coverage, not journey reachability** — which is exactly why the gap was invisible in the tracker.

**Verified central blocker** (multi-agent audit, 2026-06-21): the daemon *can* capture an arbitrary working tree (`Daemon::capture_working_tree`, `crates/spork-daemon/src/feature.rs`) and `Command::NodeCreate` accepts a root snapshot-owning node — but **these are reachable only from tests/examples**. `open_project` roots the daemon at an empty `root/work` subdir (`crates/spork-daemon/src/core.rs:326`) and never ingests the user's files. There is **no capture/import IPC command** in the frozen `Command` enum, and the renderer "cannot mint a content hash" so it cannot call `NodeCreate` for a root. Net: empty canvas, forever.

**A correction to an earlier claim:** the code-mutating agent is **not** wholesale-P8. The plan over-defers by conflating *(a)* a **trusted-local** edit loop — the agent edits *your* code in a CoW worktree you already trust — which the shipped `WorktreeCowBackend` is explicitly "right for" (`crates/spork-exec/src/backend.rs`), with fork-on-divergence already implemented (`spork-daemon/src/checkout.rs`) and the transcript already modelling `ToolCall`/`ToolResult` — with *(b)* **untrusted-marketplace** executor isolation (microVM/WASM), which *is* P8. Only (b) needs P8.

---

## 2. MVP definition & the user journey (the Definition of Done, in user terms)

**MVP definition.** A single-user, local-first, **trusted-agent** IDE where a user points Spork at an existing repo, sees their code as a root node on the DAG canvas, asks a locally-configured agent (a CLI like `copilot-cli`, or a local OpenAI-compatible HTTP endpoint) to do real work that **edits the code**, sees the result as a new **Edit** node (diff + transcript + auto-Sanity) that auto-forks on divergence, inspects/run-checks/branches/merges/restores it, and re-opens the project later to find the graph intact. It uses **only** the shipped F4 `WorktreeCowBackend` (trusted tier) plus additive IPC/UI wiring.

**The journey the MVP must make real (each step is a DoD line):**

| # | Step | Today | After P7.5 |
|---|---|---|---|
| 1 | Point Spork at an existing repo dir | roots at empty `…/work` | daemon roots at the user's actual project |
| 2 | See your code as a root node | empty canvas, no path | working tree captured → root snapshot node renders |
| 3 | Configure an agent once (CLI `copilot-cli` or local endpoint) | hardcoded local default, no UI | Settings surface feeds a live `AgentConfig` |
| 4 | Select a node, ask the agent to **make a change** | only `ask`/`plan`/`analysis` (read-only) | new `change` intent |
| 5 | Daemon edits in a CoW worktree, snapshots the result | not wired | `WorktreeCowBackend` provision→tool-loop→capture |
| 6 | New **Edit** node, fork-on-divergence, auto-Sanity | not wired | wired (reuses §6.6 + auto-run Sanity) |
| 7 | Inspect diff + agent transcript + Sanity result | diff ✓; transcript ✗ | conversation/transcript surfaced |
| 8 | run-check / branch / merge / gated-merge / restore | ✅ already wired | unchanged |
| 9 | Close & reopen — graph intact | ✅ (rebuild from log) | unchanged |

---

## 3. Scope boundaries

**In scope (all wiring-gaps — backend exists, expose it):** W1 onboarding/import · W2 root-at-real-dir · W3 provider config · W4 trusted edit loop · W5 conversation surface.

**Explicitly deferred (genuinely new infra — NOT MVP-blocking, documented out-of-scope per C1):**

| Deferred item | Why it's not MVP | Lands |
|---|---|---|
| **Untrusted executor isolation** (Container/MicroVm/WASM tiers, untrusted-code escalation) | MVP uses only the trusted host-kernel `WorktreeCow` tier; running *third-party* node code safely is a separate concern | **P8** (real) |
| **Cloud providers over TLS** (`api.anthropic.com`/`openai.com`) | additive over the frozen `Transport` seam (`https` is refused today, `spork-transport/src/http.rs`); the wedge is local-first (CLI + local HTTP already work) | post-MVP, additive |
| **Live token streaming** over `CHAT_TOKENS` | the rail is plumbed end-to-end already; non-streaming completed-answer runs work — streaming is polish needing a streaming `Transport` method + SSE producer | post-MVP, additive |

These are recorded so their omission is a **decision, not an oversight** (C1).

---

## 4. The seams we build on (all shipped F0–F4 — nothing new is frozen)

| Capability the MVP needs | Already shipped | Where |
|---|---|---|
| Capture an arbitrary working tree → `(snapshot_hash, root_tree)` | `Daemon::capture_working_tree` | `spork-daemon/src/feature.rs` |
| Content-address an arbitrary dir | `ObjectStore::capture_snapshot` | `spork-cas/src/store.rs` |
| Import modeled as data (`origin=import`) | `SnapshotPayload::import` / snapshot kind | `spork-nodes/src/snapshot.rs` |
| Create a root snapshot-owning node | `Command::NodeCreate` (parents may be empty) | `spork-ipc/src/command.rs`, dispatch in `spork-daemon/src/dispatch.rs` |
| CoW worktree provision / exec / capture (trusted tier) | `WorktreeCowBackend::{provision, exec, capture_path}` | `spork-exec/src/worktree.rs`, `backend.rs` |
| Fork-on-divergence (§6.6) | checkout/auto-branch helper | `spork-daemon/src/checkout.rs` |
| Auto-run Sanity after an edit | `auto_run_sanity` | `spork-daemon/src/nodes.rs` |
| Provider transport (local HTTP + CLI JSONL) | `HttpTransport`, `SubprocessTransport` | `spork-transport/src/{http,subprocess}.rs` |
| Agent turn (resolve→render→price), fallback | `run_turn` | `spork-agent/src/lib.rs` |
| Conversation as content-addressed transcript | `CanonicalTranscript`, `put_conversation`, dual-restore | `spork-provider`, `spork-daemon/src/feature.rs`, `spork-restore` |
| Read a node's transcript (MCP/history) | `get_node_transcript` (expects `conversation_ref`) | `spork-history/src/{history,mcp}.rs` |

Everything below is **glue + UI** over these.

---

## 5. Workstreams

Each workstream lists **Backend** (exact files, IPC/Tauri additions, schema/capability notes), **UI** (components, store, IPC types), **Design refs**, and **DoD**. All `Command` additions are **append-only new variants** (the enum carries a frozen *wire form*; new variants are additive per C2/C3/D-8). Persisted payloads carry `schema_version` (C5).

---

### W1 — Onboarding & Import (capture the repo into the first node)  · *wiring-gap · effort M · the unblocker*

**Goal (user):** open a repo and immediately see it as a root node on the canvas.

**Backend**
- **New IPC command** (append after `HistoryQuery` in `crates/spork-ipc/src/command.rs`, preserving the frozen wire form):
  ```
  Command::ProjectImport {
      branch_id: String,            // default "main"
      origin: "manual" | "import",  // maps to SnapshotOrigin
  }
  ```
  It is a **mutation** (returns `CommandResult::Mutation` with `{ nodeId }`). No new `OpLogEvent` — it reuses `NodeCreated`.
- **Dispatch arm** in `crates/spork-daemon/src/dispatch.rs` → new `cmd_project_import` (new module `spork-daemon/src/import.rs`):
  1. authorize `Capability::SnapshotWrite` (scope `WORKTREE_GLOB`);
  2. call the existing `self.capture_working_tree()` (`feature.rs`) → `(snapshot_hash, root_tree)`;
  3. build a `SnapshotPayload` (`origin = import|manual`; for git repos, attach `import_git_state` metadata via `ImportSource`);
  4. create a **root** node through the existing `create_node` path: `kind = SNAPSHOT_KIND`, `parent_ids = []`, `branch_id`, `owns_snapshot = true`, `snapshot_hash`;
  5. create the `branch/<branch_id>` + `HEAD` refs (reuse `create_ref`/`move_ref`) so the node is a branch tip;
  6. emit the existing `NodeCreated` (+ `RefCreated`) events; return `{ nodeId }`.
- **Idempotency / re-capture:** a second `ProjectImport` on an unchanged tree dedups by hash (content-addressing) — capturing again yields the same `snapshot_hash`; if the tree changed, it creates a new drift/import snapshot child (reuse the §6.6 policy from W4). Document this; v1 may simply refuse a duplicate root if one exists on the branch.
- **Capability/secret note:** capture already secret-scans at the CAS boundary (F3) — a planted key never enters the store (DESIGN §15.4).

**UI**
- `app/src/ipc/types.ts`: add `PROJECT_IMPORT` command type.
- `app/src/canvas/Canvas.tsx` `EmptyCanvas` (currently only `open_project`): after `openProject(path)` resolves, dispatch `PROJECT_IMPORT` then invalidate `GRAPH_VIEW_KEY`; replace the "the UI cannot mint a content hash" copy with the real flow ("Importing your project…").
- `app/src/app/TopBar.tsx`: add an **"Import project"** action (also reachable from the command palette) for re-capture / opening another repo.
- Activity log line on success ("Imported <path> — root node <id>").

**Design refs:** DESIGN §10.1–§10.5 (capture/identity), §6.2 (snapshot kind), A.7 C-2 (import = snapshot `origin=import`); UI_UX_DESIGN §5.4 (empty state), and flip its "no capture IPC command" notes (§ around the honesty matrix) to ✅ on completion.

**DoD:** open a real repo → a root snapshot node renders within the capture budget; its diff/blob reads work; reopening the project rebuilds it from the log.

---

### W2 — Root the daemon at the user's real project dir  · *wiring-gap · effort S · correctness fix*

**Goal:** the captured tree is the **user's code**, not an empty scratch dir.

**Backend**
- `crates/spork-daemon/src/core.rs:326` currently does `let workdir = root.join("work")`. Two viable designs (pick one in implementation):
  - **(a) Separate state dir (recommended):** keep CAS/log/vault under a `.spork/` subdir of the project (`root/.spork/{cas,log.db,vault}`) and set `workdir = root` (the user's repo itself). The ignore profile already excludes `.spork/`-style state and deps (F0 `ignore_profile_hash`), so capture sees the user's files and not Spork's own store.
  - **(b) Explicit project path:** add a `DaemonBuilder::with_workdir(path)` and have `open_project` pass the selected repo as the workdir while keeping state under a sibling/hidden dir.
- Ensure the **ignore profile** excludes the Spork state dir + dependency dirs so capture is fast and clean (DESIGN §10.5; F0 deps-excluded policy).
- This touches a builder default, not a frozen contract — additive.

**UI**
- `app/src-tauri/src/lib.rs` `open_project`: pass the user-selected path through as the project root/workdir; surface a clear error if the path is unreadable.
- Onboarding copy clarifies that Spork keeps its state in a hidden `.spork/` dir inside the project (and that it is git-ignored / never committed).

**Design refs:** DESIGN §10.1 (three-layer architecture), §10.5 (exclusions), §15.4 (secrets/scan).

**DoD:** opening `/my/repo` captures `/my/repo`'s files (not an empty dir); Spork's own store lives under `/my/repo/.spork/` and is excluded from snapshots.

---

### W3 — Provider / agent configuration from the app  · *wiring-gap · effort M*

**Goal:** a user points Spork at `copilot-cli` (or a local endpoint) **from the app**, once.

**Backend**
- `AgentConfig` (`crates/spork-daemon/src/agent.rs`) is a plain struct (`local_endpoint`, `cli_command`). Add builders: `with_cli_agent(program, args)` and `with_endpoint(url)` (alongside the existing `with_default_local`).
- Add a **runtime setter** `Daemon::set_agent_config(&self, AgentConfig)` (mutates the `agent_config` field under the core lock) so the provider can change without reopening the project. Also widen the `net.connect` grant to the configured host when an endpoint is set, and ensure `process.spawn` is granted for a CLI command (both already in `default_grants`/`grant_model_access`; verify scope).
- **New Tauri command** `set_agent_config` in `app/src-tauri/src/lib.rs` (app-local command, *not* a `spork-ipc::Command` — consistent with the existing `ping`/`open_project`/`graph_view`/`dispatch` Tauri surface, so the frozen IPC boundary is untouched). Register in `invoke_handler`.
- Persist the choice (a small JSON under `…/.spork/agent_config.json`) so it survives restart; load it at `open_project`.

**UI**
- `app/src/app/overlays/SettingsPopover.tsx` (today: theme/density/layout/default-model): add a **Provider** section — radio between "Local HTTP endpoint" (URL field, default Ollama) and "CLI agent" (command + args, e.g. `copilot-cli`); a "Test connection" button (dispatch a trivial read-only `ask`).
- Gate the top-bar **model selector** (`app/src/app/TopBar.tsx`) so it only advertises providers that are actually configured (don't show `cli/copilot-cli` unless a CLI command is set) — closes the "UI advertises an unbacked provider" honesty gap.
- `app/src/ipc/client.ts`: add `setAgentConfig(cfg)` wrapper; `mock.ts`: a no-op mock.

**Design refs:** DESIGN §12.1–§12.5 (provider abstraction, privacy classes), §15.1 (trust boundary — renderer holds zero secrets; a CLI command/endpoint is config, not a secret); UI_UX_DESIGN settings + model-selector surfaces.

**DoD:** with `copilot-cli` (or a local server) configured in Settings, a read-only `ask` returns a real answer; the model selector never offers an unconfigured provider; the choice survives restart.

---

### W4 — Trusted-local code-mutating edit loop  · *wiring-gap (trusted tier only) · effort L · the headline capability*

**Goal:** ask the agent to change the code; get a real Edit node.

**Backend**
- **New intent:** add `AgentRunIntent::Change` (the enum is `#[non_exhaustive]` in `crates/spork-ipc/src/command.rs` — additive, safe).
- **New IPC command** `Command::NodeAgentEdit { target_node_id, prompt, model_key, privacy }` (append-only). Kept separate from the frozen read-only `NodeAgentRun` so that command's attach-a-context-node semantics are untouched (C2). Mutation → returns `{ editNodeId, branchId, forked, model, costMicroUsd, sanity }`.
- **Daemon holds a `WorktreeCowBackend`** (objects + assets + lease ledger already live in `DaemonCore`); construct once in `build`. New module `spork-daemon/src/edit.rs` → `cmd_node_agent_edit`:
  1. authorize `model.invoke` (+ `net.connect`/`process.spawn` per transport) and `snapshot.write`;
  2. resolve the parent snapshot; `backend.provision(parent_snapshot, env)` → a CoW `Workspace` under a TTL lease;
  3. run **`spork_agent::run_agent_loop`** (new): after `run_turn`, execute each `ContentBlock::ToolCall` against the workspace — a small **trusted tool set** (`read_file`, `write_file`, `list_dir`, `run_command` via `backend.exec`), appending `ToolResult` to the transcript — iterating until the model emits no more tool calls (bounded by a max-iterations guard);
  4. `backend.capture_path(ws, ws.root)` → the mutated tree → wrap as a snapshot (`put_snapshot_over`, exists in `nodes.rs`);
  5. build an `EditPayload` (`diff_summary`, `files_changed`, `tool_calls`, `conversation_ref` ← `put_conversation(transcript)`); create a `codebase-edit` node owning the new snapshot, parented on the target;
  6. apply **§6.6 fork-on-divergence** via the existing `checkout.rs` helper (tip → continue branch; non-tip → auto-fork);
  7. **auto-run Sanity** via the existing `auto_run_sanity` on the changed paths;
  8. tear down the lease; return the ids.
- **Capabilities/safety:** uses **only** the host-kernel `WorktreeCow` tier ("right for trusted edits", `backend.rs`) against a CoW copy — *never the user's real checkout* (DESIGN §9.2, §15.3). Untrusted isolation stays out (P8). `run_command` is gated by `process.spawn`; document that the MVP trusts the *user-configured* agent.
- **Schema:** `EditPayload` already versioned (P5); no new persisted schema beyond the additive command/intent.

**UI**
- `app/src/app/overlays/Modals.tsx` Ask-agent modal: add a **"Make a change"** intent (maps to `NodeAgentEdit`); show a clear "this will edit a copy and create an Edit node" note.
- After success: the new Edit node appears (live op-log fold), auto-selected; the diff panel (already built) shows the change; the run rail shows the auto-Sanity result.
- `app/src/ipc/types.ts`: `NODE_AGENT_EDIT` command + `change` intent; `mock.ts`: a mock that fabricates an Edit node so the browser-preview path still renders.

**Design refs:** DESIGN §6.6 (fork-on-divergence), §8.2 (auto-run Sanity), §9.2 (CoW-only mutation, capabilities), §11.1 (IsolationBackend trusted tier), §13.4 (code+conversation binding), §15.3 (untracked-mutation gap closed at the execution layer).

**DoD:** select a node → "make a change" → an Edit node owning a new snapshot appears with a real diff, a stored transcript, and an auto-Sanity result; from a non-tip node it lands on an auto-forked branch; the user's real working dir is untouched.

---

### W5 — Conversation & transcript surface  · *wiring-gap · effort M*

**Goal:** see the agent's conversation/transcript on a node; thread multi-turn.

**Backend**
- **Fix the transcript read gap:** `cmd_node_agent_run` stores `payload.answer` inline (`agent.rs`), but `get_node_transcript` expects `payload.conversation_ref` (`spork-history/src/history.rs`) → it returns `None`. For both the read-only ask and the W4 edit, **store the `CanonicalTranscript` via `put_conversation` and put its `conversation_ref` on the node payload**, so the existing `HistoryQuery → get_node_transcript` path (and the History MCP) returns it.
- **Multi-turn read-only chat:** build the agent input as a `CanonicalTranscript` walking prior context-node children of the target (instead of the single `user_prompt` in `agent.rs`), so a thread accumulates. All additive over frozen transcript/restore seams (the transcript already dual-restores with code, DESIGN §13.4).

**UI**
- Enable the disabled **"Run output" / Conversation tab** in `app/src/app/RunRail.tsx` (currently `disabled`, lines ~55–63) — render the already-buffered per-node ephemeral rail (`store.ts` `ingestEphemeral`).
- Add a **Conversation view in `NodeDetails`** that renders the node's transcript (via a `useNodeTranscript` query over `HistoryQuery`/`get_node_transcript`) + a composer that dispatches a follow-up `NodeAgentRun` (read-only) threaded on the node.

**Design refs:** DESIGN §13.4 (code+conversation binding), §13.7 (Lineage/History MCP), §12.3 (canonical transcript); UI_UX_DESIGN Conversation surface (currently 🟡 — flips to 🟢 on completion).

**DoD:** a node's transcript renders in Node-Details; a follow-up message threads onto the node and is retrievable via the History MCP.

---

## 6. Sequencing — two slices

- **Slice A — "Usable today" (W1 + W2 + W3 + the W5 transcript-read fix):** S/M effort. Delivers **open repo → see code → configure agent → read-only ask → run checks → branch/merge/restore**. This alone removes the empty-canvas dead-end and makes the app demonstrably useful. *Recommended first.*
- **Slice B — "Agent does work" (W4 + W5 chat/threading):** L effort. Delivers the **code-mutating edit loop + conversation surface** — the headline "agent tries possibilities across branches" capability (single-agent; parallel multi-branch exploration is a thin loop on top, or P8 with tiered executors).

Each slice ends with the standard phase ritual: workspace green bar (`cargo test/clippy/fmt`, app `tsc`/vitest/build), the UI_UX honesty-matrix re-tag, and a `TODO.md` update.

---

## 7. Cross-cutting constraints (held throughout)

- **C1 no stubs:** every workstream ships a complete slice; the three deferred items (§3) are explicit documented out-of-scope, not placeholders.
- **C2 no domino:** no frozen contract edited — only **additive** `Command`/`AgentRunIntent` variants (append-only / `#[non_exhaustive]`), new Tauri commands, new daemon methods/modules, new UI.
- **C3 additive behind seams:** the edit loop is a new consumer of the frozen `IsolationBackend`/`Runner`/`ProviderAdapter`/`Transport` seams; no core edit.
- **C4 design-consistent:** every task cites a DESIGN § it realizes; none contradicts the spec (it operationalizes §17).
- **C5 evolution-safe:** persisted payloads keep `schema_version`; no stored-event rewrite (import/edit reuse existing versioned payloads).
- **Security:** all mutation flows through CoW copies, never the real checkout (§9.2/§15.3); secrets stay `vaultRef`-only and secret-scanned at capture (§15.4); capabilities deny-by-default, granted explicitly; the renderer holds zero secrets (§15.1).

---

## 8. Test plan (the falsifiable MVP DoD)

1. **Onboard:** headless + Tauri-backend test — `open_project(repo)` then `ProjectImport` → a root snapshot node exists, its diff lists the repo's files, reopen rebuilds it. (extends `headless_dod.rs`)
2. **Root-at-real-dir:** capture over a fixture repo captures the fixture's files and excludes `.spork/`.
3. **Provider config:** `set_agent_config(cli/local)` → a read-only `ask` returns an answer through the configured transport; persisted across reopen.
4. **Edit loop:** `NodeAgentEdit` against a fixture (with a scripted local/CLI agent) → an Edit node owns a new snapshot with the expected diff; from a non-tip node it auto-forks; auto-Sanity result attached; real workdir unchanged.
5. **Conversation:** `get_node_transcript` returns the stored transcript for an ask and an edit; a threaded follow-up appends.
6. **App:** Vitest covers the EmptyCanvas import flow, the Settings provider form, the change-intent modal, and the Conversation view; `tsc` + `vite build` green.
7. **Green bar:** full `cargo test --workspace` / `clippy -D warnings` / `fmt --check`; no stub macros.

---

## 9. Doc-update checklist (done as each slice lands)

- [ ] [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md): **P7.5 (MVP Readiness)** section + a milestone-table row, both pointing here. *(added now, as a pointer)*
- [ ] [TODO.md](TODO.md): a **P7.5** section mirroring §5 workstreams (unchecked), and an honesty note that "all core built" meant contract coverage, not journey reachability. *(added now)*
- [ ] [DESIGN.md](DESIGN.md) §17: a note that "Phase 1 — Local MVP" is operationalized by this plan + P7.5. *(added now)*
- [ ] [UI_UX_DESIGN.md](UI_UX_DESIGN.md): an MVP-surfaces pointer to this plan; on completion, re-tag the onboarding/import/provider-config/edit/conversation surfaces 🟡→🟢 and re-check the honesty matrix (§16 ritual). *(pointer added now; tags flip on build)*
- [ ] [CLAUDE.md](../CLAUDE.md): add `MVP_PLAN.md` to the docs table. *(added now)*

---

## 10. Open questions for the reviewer (you)

1. **State-dir layout (W2):** keep Spork's CAS/log/vault in a hidden `.spork/` inside the repo (recommended) vs. a separate location? (affects capture exclusions + git-ignore guidance)
2. **Edit-loop tool set (W4):** start with `read_file`/`write_file`/`list_dir`/`run_command`, or also `apply_patch`/grep? And a max-iterations + cost ceiling per edit run — what defaults?
3. **Provider persistence (W3):** per-project `…/.spork/agent_config.json` (recommended) vs. a global app config?
4. **Slice order:** ship **Slice A** first (recommended) and demo "open → see → ask → check → branch/merge", then **Slice B** (agent edits)? Or one combined push?
5. **"Multiple possibilities" (your phrase):** single-agent edit + manual branching (MVP), or a built-in "fan out N edit attempts on N branches" helper (a thin loop on W4, still trusted-tier — could be in scope)?
