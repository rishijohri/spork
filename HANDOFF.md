# Spork — Session Handoff

> **Read order for a fresh session:** this file first (orientation + current state) → [`CLAUDE.md`](CLAUDE.md) (rules) → [`docs/REALIGNMENT_PLAN.md`](docs/REALIGNMENT_PLAN.md) (the active plan) → [`docs/TODO.md`](docs/TODO.md) (live progress). Do **not** start coding until you've internalized §1 (the vision) and §5 (the hard constraints).
>
> **Branch:** `feat/f3-ui-redesign` (PR #1). **State:** MVP built + bug-fixed; realignment **R0 (docs) + R1 (connectivity unblock) + R2 (view fields + de-git-ified UI + hero chat + open-in-editor) DONE & green** — **all uncommitted**. Next is **R3** (node-type descriptors + dispatch + the `presentation_status` producer), which wants OD-2/OD-4 confirmed first. **Commit/branch only when the user asks.**

---

## 1. The core idea — keep everything grounded here

Spork is **NOT** a git tool, **NOT** a code editor, and **NOT** a model host. The MVP UI drifted git-ward (a branch sidebar, a branch switcher, merge/push as primary verbs) — that drift is the thing we are correcting. The real product, in the user's own framing:

> **Spork is the branching-timeline + sandbox + agent-orchestration layer that sits *on top of* the agents and editors a single developer already uses on a single machine to build complex codebases with AI.**

The load-bearing ideas — treat these as the source of truth for every design and UI decision:

1. **Nodes are the spine. A "branch" is an emergent *line*, never git chrome.** Every chat message / agent interaction and every deterministic action is a **node** in the timeline. Clicking any old node and starting a new conversation spawns a new node *from that point* → a new emergent line (the daemon's fork-on-divergence handles it). The user never names, switches, or merges a "branch." Internally `branchId`/`HEAD`/`refs` survive **only** as tip/fork bookkeeping; they are never user-facing.
2. **Every node carries its own sandboxed codebase** (content-addressed CoW copy) for the agent to explore/work on — so work from an earlier node doesn't require the whole project to have moved forward.
3. **Node *types* are the heart, in two families:**
   - **Agentic** — Planning / Ask / Exploratory / Working — each an agent "mode" differing by its `{instructions, skills, tools, mcp_servers}` (like VS Code agent modes).
   - **Deterministic action** — run-tests+collate, stress, sanity, git-push, git-commit — repetitive actions as first-class nodes recording shell output + result.
4. **Per-type states.** Agentic: `awaiting-input / thinking / working / complete / require-review / require-more-info`. Action/testing: `pending / running / pass / failed`.
5. **Clicking a node → dig deep:** the codebase at that point, the chat ancestry that led to it, the ancestry tree (other lines), per-type detail (agentic: instruction + agent config + what it did; action: recorded shell output + result), and the actions you can take (create new nodes from here).
6. **Platform inversion — your existing agent drives Spork.** A **write-capable Orchestration MCP** (the "Spork Node skill") lets an orchestrator agent on the user's own provider (Claude Code, Copilot, Cursor) create and run Spork nodes. A few node types (Planning, Ask, Agentic) ship available from the start.
7. **A beautiful in-app chat** is a first-class hero surface — but it **creates nodes**, it is not a flat conversation scroll. Chat and canvas are two views of the same node substrate.
8. **"Open codebase" hands off to the real IDE** (VS Code / Cursor) via open-in-editor, exactly like Claude Desktop / the Copilot app. In-app Monaco is read-only diff viewing only.

**The three user goals** these serve: (1) trace the branching conversations that shaped a codebase; (2) let an agent *optionally* browse node ancestry to understand changes **without overfilling its KV-cache** (the P7 `LineageCompiler` already compiles only the needed lineage); (3) make the user more productive with the agents they already have.

> If a proposed change reintroduces git-branch framing as a primary surface, or makes Spork into a code editor / model host, it is **wrong** — re-read this section.

---

## 2. Authoritative docs (where detail lives)

| Doc | Role for this work |
|---|---|
| [`docs/REALIGNMENT_PLAN.md`](docs/REALIGNMENT_PLAN.md) | **THE active plan.** The reframed model, the connectivity decision, the additive node-type + state spec, the Orchestration MCP, the timeline-first UI reframe (incl. hero chat + open-in-editor), the phased plan R0–R6, and a §7 **C2 risk register** (adversarially audited). Start here. |
| [`CLAUDE.md`](CLAUDE.md) | Project rules (C1–C5), the positioning paragraph, the build spine, how to work in the repo, verify-green discipline. |
| [`docs/TODO.md`](docs/TODO.md) | Live progress tracker. The **Realignment (R0–R6)** section has the checklist + the adopted open decisions. P7.5 (MVP) section shows what's built. |
| [`docs/IDEA.md`](docs/IDEA.md) | Durable vision (six dimensions). Updated with the realignment note + reframed non-goals. |
| [`docs/DESIGN.md`](docs/DESIGN.md) | Authoritative spec (19 §). Has a realignment note up top; cite its § numbers. The realignment **refines, never contradicts** it. |
| [`docs/MVP_PLAN.md`](docs/MVP_PLAN.md) | The P7.5 MVP plan (Slices A+B) — what was just built. |

---

## 3. What is actually built right now (the codebase state)

**Foundation F0–F4 + P5–P7 complete** (35 crates; the frozen-contract substrate). On top of that, the following are **built, green, and COMMITTED on `feat/f3-ui-redesign`** (the P7.5 MVP + bug-fixes + R0 docs + R1 + R2 were committed together this session, split backend-Rust+docs / React-frontend — see `git log`):

**C. Realignment R1 + R2** (this session) — see `docs/TODO.md` §Realignment + `docs/REALIGNMENT_PLAN.md`:
- **R1 connectivity unblock** — soft-deprecated the generic-CLI-as-model route (defaults purged + `CLI_AS_MODEL_GUIDANCE`/`map_agent_error_for` loud-guidance failure path); local-HTTP is the default. Seams untouched.
- **R2 timeline-first reframe** — additive `NodeView.presentationStatus`/`lineLabel`/`forkedFrom` (schema v2); LINES navigator + swimlanes (`ViewportPortal`) + state badges; the **hero chat** (`app/src/app/HeroChat.tsx`); family-driven Node-Details; **open-in-editor** (`open_in_editor` Tauri cmd + Settings Editor pref).

**A. The Local Trusted-Agent MVP (P7.5 Slices A + B)** — see `docs/MVP_PLAN.md` / `docs/TODO.md` §P7.5:
- **Onboarding/import** — `Command::ProjectImport` (+ `cmd_project_import` in `crates/spork-daemon/src/import.rs`) captures the working tree → a root snapshot node. **Now idempotent on reopen.**
- **Root-at-real-dir** — `DaemonBuilder::for_project` roots the daemon at the user's repo; state under hidden `<project>/.spork/`.
- **Provider config** — `AgentConfig` builders + `Daemon::set_agent_config` + a Tauri `set_agent_config` command + `<project>/.spork/agent_config.json` persistence + a Settings provider form. *(NOTE: the generic-CLI-as-model route here is what the realignment deprecates — it's the source of the "Test connection" errors.)*
- **Trusted edit loop** — `Command::NodeAgentEdit` + `crates/spork-daemon/src/edit.rs`: provisions a CoW copy via `WorktreeCowBackend`, runs a bounded agent tool-loop (read/write/list/run-command/apply_patch, 20 iter / $5 cap), captures the mutated tree → a `codebase-edit` node, applies §6.6 fork-on-divergence + auto-Sanity. The user's real checkout is never touched.
- **Conversation/transcript** — agent runs bind `conversation_ref`; History MCP `get_node_transcript` returns it; a Node-Details Conversation tab + the "Make a change" intent in the Ask modal.

**B. Post-launch bug fixes** (found via a 9-agent adversarial UI audit) — see `docs/TODO.md` §P7.5 "Post-launch fixes":
- Native **folder picker** (Tauri `dialog` plugin — `app/src-tauri` now depends on `tauri-plugin-dialog`; **a fresh `tauri dev` recompile is required** to pick it up).
- **Settings popover** off-screen fix (it was double-absolute).
- **Reopen** a project no longer errors (idempotent import + always-refetch).
- **Project persistence + auto-reopen** (localStorage; `projectOpening` gates an "Opening…" spinner).
- **Context-menu** viewport clamp; **inline onboarding errors**.
- **Deferred (known limitation):** large-repo import freezes the UI (synchronous capture on the owner thread).

**Green bar as of the last full run (R1+R2, committed):** **1163** workspace + **96** app (Vitest) + **10** Tauri tests; clippy `-D warnings` + rustfmt + `tsc` + `vite build` clean; no stub macros. Re-verify before claiming green (cargo is off PATH — `export PATH="$HOME/.cargo/bin:$PATH"`).

---

## 4. The realignment plan (R0–R6) — what's next

Full detail in `docs/REALIGNMENT_PLAN.md` §6. **R0 + R1 + R2 done & green.** R3–R6 proposed, **not built**.

- **R0 — Docs realignment** ✅ (this realignment; no code).
- **R1 — Connectivity unblock** ✅ **DONE** — soft-deprecated the generic-CLI-as-model route: unwired the fictional `copilot-cli` from router/registry/UI defaults, `CliAdapter::DEFAULT_MODEL` → neutral `"cli-agent"`, added `CLI_AS_MODEL_GUIDANCE` + `map_agent_error_for` so the CLI failure path **fails loud with guidance** (run + edit loops); local-HTTP is the visible default. Frozen `Transport`/`ProviderAdapter`/`CLI_PROVIDER_KEY` seams untouched — a *conforming* JSONL program still resolves (DoD tests green).
- **R2 — View fields + UI de-git-ification + hero chat + open-in-editor** ✅ **DONE** — additive `NodeView.presentationStatus`/`lineLabel`/`forkedFrom` (`presentationStatus` kind-gated, `None` until R3's producer), schema v2 (no migration); LINES navigator + swimlanes (`ViewportPortal`) + state badges (`statusBadge`); line breadcrumb + canvas⇄chat toggle; "+ New node from here" menu (Agentic/Action), merge→line, removed NewBranch/BranchMenu; the **hero chat** (`HeroChat.tsx` — messages are nodes); family-driven Node-Details (Thread/Result); **open-in-editor** (`open_in_editor` Tauri cmd + Settings Editor pref). Green: 1163 ws + 96 app + 10 Tauri.
- **R3 — Agentic + action node-type descriptors + dispatch + the state producer.** *(Next; needs OD-2/OD-4.)*
- **R4 — git-push/commit as real nodes** (a *new* node-producing command — never edit the frozen `GIT_PUSH`/`GIT_EXPORT`).
- **R5 — The write-capable Orchestration MCP** (`spork-orchestrate` crate + `Command::OrchestrateInvoke` + a stdio shim binary + the "Spork Node skill" doc).
- **R6 — Cloud BYOK TLS transport** (a new `TlsHttpTransport` behind the frozen `Transport` seam + a `vaultRef` on `AgentConfig`).

**Connectivity decision (locked):** two arrows — *Spork drives a model* = local OpenAI-HTTP (default) + cloud BYOK over TLS (R6); *your agent drives Spork* = the Orchestration MCP (R5). The generic CLI-as-model provider is deprecated (it pipes Spork's private JSONL into `claude`/`copilot`, which nothing speaks).

---

## 5. Hard constraints a fresh session MUST respect

- **C2 — no domino.** Every realignment change is **additive over frozen F0–F4/P5–P7 contracts**. Do not edit a frozen contract in place. The §7 risk register in `REALIGNMENT_PLAN.md` lists the traps; the load-bearing ones, verified against source:
  - There is **no `mode` field** — agent runs carry `intent: AgentRunIntent {Ask,Plan,Analysis,Change}`. The agentic *type* (`kind`) maps onto `intent` (`Exploratory→Analysis`); new dispatch stamps the `kind` + richer payload. *(Not a free reuse.)*
  - `EditPayload` is a **closed struct** — the Working node gets a new `agent-work` descriptor (not an `EditPayload` mutation).
  - The frozen `Lifecycle` enum is untouched — per-type states ride as a **`presentation_status` in the node payload**, surfaced via an additive `NodeView` field read with `get_payload` (like `gate_verdict_view`). **`require_review → Blocked`** (NOT green — green would make the scheduler advance dependents past human review).
  - `presentation_status` has **no producer yet** → it ships forward-mapped (🟡, `None`) until the R3 agent-loop producer lands.
  - **git-push/commit emit no `OpLogEvent`** today → they become nodes only via a *new* command, never by editing the frozen variants.
  - `GraphView` is a transient projection → add `Option` fields + bump its schema version, **no migration**.
  - The TLS transport is **unbuilt** (only `http.rs`/`subprocess.rs` exist) → R6 is real new work, not a flag-flip.
- **C1 no stubs · C3 additive behind seams · C4 cite DESIGN § · C5 schema_version + migration on persisted structs.**
- **Verify before claiming green** — yourself, from repo root: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`; app: `tsc`/vitest/`vite build`; Tauri: `cd app/src-tauri && cargo check`. Don't trust a subagent's self-report.
- **Paired UI rule (CLAUDE.md §8)** — a backend change with a UI consequence updates `docs/UI_UX_DESIGN.md` (and re-tags 🟡→🟢 on completion) then the UI.
- **Secrets** only via `vaultRef` inside the daemon; the renderer holds zero secrets.

---

## 6. Open decisions — all resolved

1. **OD-1** — agentic `kind` → existing `intent`, no parallel `mode`. *(Adopted.)*
2. **OD-2** — `require_review → Blocked` (dependents stall until a human acts). ✅ **USER-CONFIRMED 2026-06-22** — locked for R3.
3. **OD-3** — new `agent-work` descriptor over an `EditPayload` migration. *(Adopted.)*
4. **OD-4** — one global "allow external orchestrator" checkbox for now; per-client scoping later via the §15.1 grammar. ✅ **USER-CONFIRMED 2026-06-22** — locked for R5.
5. **OD-5** — ship R1 + R2 first. *(Done.)*

No open decisions block R3+.

---

## 7. Immediate next action

**R0–R2 are done, green, and committed. OD-2 + OD-4 are user-confirmed. The next session BUILDS R3 — no gates remain.**

1. **Build R3** — register the agentic (`agent-plan/ask/explore/work`) + deterministic-action (`action-run-tests/stress/sanity`) **node-type descriptors** through the public `register_descriptor` path (exactly as `agent_context_descriptor()`/`gate_descriptor()` do — `crates/spork-daemon/src/agent.rs`); add the **new daemon dispatch** that stamps a per-kind `kind` + the rich payload (config in the payload, mapped onto the existing `AgentRunIntent` — `cmd_node_agent_run` today hardcodes `AGENT_CONTEXT_KIND`, so this is new additive code, NOT a free reuse); ship the new **`agent-work`** descriptor (OD-3, not an `EditPayload` migration); and add the **`presentation_status` producer** in the agent loop that writes the rich state into the node payload — which **turns the R2 state badge live** (the read path is already wired: `read.rs::presentation_status_view`, kind-gated `agent-*`, returns `None` until this producer lands; the renderer's `statusBadge` already prefers it). Re-tag the state-badge surface 🟡→🟢 in UI_UX §13 when it lands.
2. **Locked decisions** (do NOT re-ask): **OD-2 `require_review → Blocked`** (the rich→Lifecycle map must send `require_review`/`require_more_info` to `Blocked`, never green — REALIGNMENT_PLAN §3c); **OD-4 one global "allow external orchestrator" gate** (for R5). Both ✅ user-confirmed 2026-06-22.
3. **C2 traps for R3** (REALIGNMENT_PLAN §3, §7): there is **no `mode` field** — the agentic *kind* maps onto `intent: AgentRunIntent {Ask,Plan,Analysis,Change}` (`agent-explore → Analysis`); `EditPayload` is a closed struct (→ new `agent-work` descriptor); `presentation_status` rides in the node's **own payload**, surfaced via the additive `NodeView` field (not an envelope column). All additive — never edit a frozen contract.
4. **Commit discipline** — work is committed; future commits only when the user asks; end messages with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. If on `main`, branch first (currently `feat/f3-ui-redesign`).
5. **To see R2 in the app:** `cd app && npm run dev` (browser-mock; the demo DAG now has a forked agent line + state badge) or `npm run tauri:dev` (**restart** required for the new Rust `open_in_editor` command).

> **North star (don't lose it):** the graph of typed nodes is the hero; Spork makes a single developer dramatically more productive with the AI agents and editor they already have — by remembering, branching, and letting their agents drive the whole timeline. It is not git, not an editor, not a model host.
