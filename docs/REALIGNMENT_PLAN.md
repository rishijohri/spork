# Spork — Realignment Plan (Timeline-First · Node-Centric · Platform-Inverted)

> **Status:** R0 (docs realignment) — **proposed, no code yet.** This is the authoritative plan for re-centering Spork on its actual thesis after the MVP UI drifted git-ward. It is the input for the IDEA/DESIGN/UI_UX edits and supersedes the git-branch framing wherever they conflict. Read it, then say "build R<n>" to start. Every change here is **additive over the frozen F0–F4/P5–P7 contracts** (C2 no-domino) — the §7 risk register proves it, audited adversarially.
>
> **Why this exists.** Running the MVP surfaced that the UI foregrounds git branches (a `BRANCHES` sidebar, a branch switcher, merge/push as primary verbs) and buries the thing that matters — the **timeline of typed nodes**. And the agent connectivity ("Test connection" → `claude exited with status 1`) is broken because the generic-CLI provider makes a coding *agent* impersonate a one-shot *model* over a private protocol nothing speaks. This plan fixes both, and builds out the agentic/action node-type model + the platform-inversion (your agents drive Spork) that the original vision ([IDEA.md](IDEA.md)) always implied.

---

## 0. Positioning (put this at the top of every doc)

> **Spork is the branching-timeline + sandbox + agent-orchestration layer that sits *on top of* the agents and editors you already use.** It connects to your agents over **MCP**, delegates code-editing to your **IDE** (open-in-editor), runs every piece of work in its own **restorable sandbox**, and remembers the whole branching history of *why* the code is the way it is. It is **not** a git tool, **not** a code editor, and **not** a model host.

The three things Spork *is*, and the three it explicitly *isn't*:

| Spork **is** | Spork is **not** |
|---|---|
| A DAG of **typed work nodes** (each = one agent turn or one deterministic action), each carrying a restorable sandbox | A git client. `branchId`/`refs`/`HEAD` survive only as internal tip/fork bookkeeping — never user-facing chrome. |
| A **platform your existing agents drive** (Claude Code / Copilot / Cursor) via a write-capable MCP | A model host. Spork drives a *model* only for its own in-app nodes (local HTTP, or BYOK over TLS). |
| A **beautiful chat + timeline** where every message becomes a node you can branch from, inspect, and replay | A code editor. "Open codebase" hands off to VS Code / Cursor; in-app Monaco is read-only diff viewing. |

---

## 1. The reframed model

**Nodes are the spine; a "branch" is an emergent consequence, not a thing the user manages.** Every agent interaction — every chat turn, every deterministic action — is a **node**. Clicking any node (tip or not) and starting new work spawns a new node *from that point*; when that new work *changes code from a non-tip node*, the daemon's existing **fork-on-divergence** policy (DESIGN §6.6, already implemented in `cmd_node_agent_edit`) automatically creates a new emergent **line**. The user never names, switches, or merges a "branch" as git chrome. Lines are drawn as swimlanes; the user reads the graph, not a branch list.

**Node *types* are the heart, in two families:**
- **Agentic** — `Planning`, `Ask`, `Exploratory`, `Working` — each a distinct registry descriptor differing only by its configuration `{instructions, skills, tools, mcp_servers, prompt}` carried in the node **payload** (like VS Code agent modes).
- **Deterministic action** — `run-tests+collate`, `stress`, `sanity`, `git-push`, `git-commit` — repetitive actions as first-class nodes that record their shell output + result.

Both register through the **same public `register_descriptor` path** the six built-ins and the P6 `agent-context` / P7 `gate` nodes already use — pure additive (C3).

**The platform inverts via MCP.** Instead of Spork spawning your agent over a bespoke protocol (the broken route), Spork exposes a **write-capable Orchestration MCP server** so your *existing* agent becomes the orchestrator that **drives Spork** — creating and running nodes, browsing lineage without overfilling its own context. This is the "Spork Node skill."

**The three user goals this serves** (from your framing): (1) **trace** the branching conversations that shaped a codebase; (2) let an agent **optionally browse node ancestry** to understand changes *without* burdening its KV-cache (the P7 `LineageCompiler` already compiles only the lineage an agent needs); (3) be **more productive with the agents you already have.**

---

## 2. The connectivity DECISION

**Root cause of the broken "CLI agent" provider — a protocol + category error, not a bad binary name.** `SubprocessTransport` pipes Spork's private `{"lines":[…]}` JSONL (`CliAdapter::to_wire`) into a spawned child and expects the same shape back. No real CLI speaks it: `copilot-cli` isn't a binary (GitHub's is `copilot`; the literal `DEFAULT_MODEL="copilot-cli"` is fictional → *No such file or directory*), and `claude` can't parse the JSONL (→ *exited status 1*). Deeper: a coding-agent CLI owns its **own** tool loop and is **not** a one-shot model — driving it through `run_turn` pits two agent loops against each other.

The two needs are genuinely different and get different routes:

| Need | Decision | Rationale |
|---|---|---|
| **(A) Spork drives a model** (its own agentic nodes run `run_turn`/`run_edit_loop`) | **Default: local OpenAI-compatible HTTP** (Ollama/LM Studio/vLLM/LiteLLM — `HttpTransport`, ships today, offline, no keys). **Additive: cloud BYOK over TLS** — a new `TlsHttpTransport` behind the frozen `Transport` seam + a `vaultRef` on `AgentConfig` (key resolved inside the daemon, renderer holds zero secrets). | Spork wants tokens + usage; HTTP gives that cleanly. **Honest gap:** the TLS transport is unbuilt (only `http.rs`/`subprocess.rs` exist) — real new work, not a flag-flip. |
| **(B) Your agent drives Spork** (the "Spork Node skill") | **Write-capable Orchestration MCP server** (new crate, mirrors the proven read-only `spork-history` `HistoryMcpServer` pattern), reached in-daemon via one new append-only `Command::OrchestrateInvoke { request }` + a thin stdio shim binary the user registers: `claude mcp add spork -- spork-mcp-orchestrate --project <path>`. | MCP is the lingua franca every target agent already speaks (Claude Code, Copilot, Cursor, Gemini all register stdio MCP servers). This *inverts the arrow*. |

**The broken generic-CLI provider is deprecated as a model route** (not "fixed"): unwire `copilot-cli` from the router/registry defaults, fix the `agent.rs` doc example (`"e.g. copilot-cli"`), and make the daemon's `cli` transport arm **fail loud with guidance** — *"this is an agentic CLI, not a model endpoint — connect it via the Orchestration MCP (your agent drives Spork), or point Spork at a local HTTP endpoint (Spork drives a model)."* The frozen `CLI_PROVIDER_KEY` / `Transport` / `ProviderAdapter` seams stay (append-only); only the defaults change. A genuine "embed a real agent CLI as a delegate" (speaking each agent's *real* `stream-json`) is future P8 work — a new node-type + executor, **never** a `ProviderAdapter`. (Note: Claude Code & Gemini have clean `stream-json`; Copilot CLI's non-interactive stdout is documented as not cleanly machine-parseable — the weakest delegate target.)

---

## 3. Node-type + state spec (fully additive — zero frozen-contract edits)

**C2 verdict (confirmed against source):** `Lifecycle` (6 variants), `Family` (3), `EdgeType`, the `NodeTypeDescriptor` shape, the view-model, and the frozen 7-capability set are **all untouched**. New kinds are new descriptors via `register_descriptor` in `register_builtins_into`, exactly as `agent_context_descriptor()` and `gate_descriptor()` already do.

### 3a. New descriptors

**Agentic family** — config `{instructions, skills, tools, mcp_servers, prompt}` lives in `payload_schema`; coarse grants in the frozen `capabilities_required: Vec<String>`:

| `id` | Family | owns_snapshot | Lowers to | Default state |
|---|---|---|---|---|
| `agent-plan` | Context | false | `NodeAgentRun{intent: Plan}` | `thinking` |
| `agent-ask` | Context | false | `NodeAgentRun{intent: Ask}` | `thinking` |
| `agent-explore` | Context | false | `NodeAgentRun{intent: Analysis}` | `thinking` |
| `agent-work` | Mutating | true (SnapshotRef out-port) | `NodeAgentEdit` | `working` |

**Reconcile the agentic *type* with the shipped `AgentRunIntent` — do NOT ship two taxonomies.** `Command::NodeAgentRun` already carries `intent: AgentRunIntent {Ask, Plan, Analysis, Change}` (there is no free `mode: String`, and no `Exploratory` intent). **Decision (OD-1):** the agentic *type* (the node `kind`) carries identity for display/config and **maps onto the existing `intent`** (`agent-explore → intent: Analysis`); `intent` stays the engine-facing axis. This needs **new daemon dispatch** to stamp a per-kind `kind` + the richer payload — `cmd_node_agent_run` today hardcodes `kind="agent-context"`. *(Audit correction: this is new additive code, not a free reuse.)*

**Deterministic action family** — `Family::Observing`, record shell output on the append-only `ResultEnvelope`:

| `id` | Lowers to | Records |
|---|---|---|
| `action-run-tests` | `NodeRunCheck{check:"validation"}` | per-unit pass/fail + stdout/stderr |
| `action-stress` | `NodeRunCheck{check:"stress"}` | metrics + output |
| `action-sanity` | `NodeRunCheck{check:"sanity"}` | output |
| `action-git-push` | **new node-producing command** (R4) | branch/commit/exit + output |
| `action-git-commit` | **new node-producing command** (R4) | commit sha + output |

### 3b. Per-type state as **presentation status** (the mechanism)

The frozen `Lifecycle` can't represent the rich agentic states. They ride as a **`presentation_status` string in the node's *own* payload**, surfaced via **one additive `NodeView.presentation_status: Option<String>`** field (skip-if-none), materialized by a **kind-gated, best-effort payload read exactly like `gate_verdict_view`** (`core.graph.get_payload(env.id)` — **not** a field on `NodeEnvelope`, which has no payload column). The renderer prefers `presentation_status` for the badge; absent → falls back to `effective_status(status, is_stale)`.

**Honesty caveat:** nothing writes these states yet. `presentation_status` is **forward-mapped and `None` for every node** until the P6 agent loop emits it (R3). Tag it 🟡 in UI_UX, same honesty pattern as the `RUN_STDOUT` Output surface.

### 3c. Lifecycle mapping (total, rich → frozen — keeps the scheduler/gates honest)

| Rich agentic state (payload) | Frozen `Lifecycle` | `effective_status` token |
|---|---|---|
| `awaiting_input` | `Pending` | Pending |
| `thinking` / `working` | `Running` | Running |
| `complete` | `Passed` | Green |
| **`require_review`** | **`Blocked`** | **Blocked** |
| `require_more_info` | `Blocked` | Blocked |
| `cancelled` | `Cancelled` | Cancelled |
| `errored` | `Failed` | Red |

**`require_review → Blocked`, never `Passed/Green` (OD-2).** Mapping it green would make the `ConstraintScheduler`/gates treat an *unreviewed* node as terminal and advance its dependents **past human review** — a real safety bug. Blocking is engine-honest: dependents stall until a human acts.

**Action nodes need NO presentation overlay** — `create_observing_node` already folds `ResultEnvelope.outcome` into Lifecycle (`pending/running/pass/failed` ≡ the frozen states). Only agentic free-form states need the payload carrier.

### 3d. "Every message = a node" + "fork from an old node"

Composes with the existing mechanism, additively: each chat turn lowers to a `NodeAgentRun` (read-only intents → a Context node attached on the current line) or `NodeAgentEdit` (a Working node that owns a snapshot and **auto-forks** when started from a non-tip). "Start a new conversation from node X" = send the next turn with X as the target → fork-on-divergence draws the new line. No new fork machinery.

---

## 4. The Orchestration MCP ("Spork Node skill")

**Shape:** a new crate `spork-orchestrate` with a pure `OrchestrateMcpServer::handle_request(&Value) -> Value` (mirrors `spork-history`), reached in-daemon via a new **append-only** `Command::OrchestrateInvoke { request }` (twin of `Command::HistoryQuery`: authorize → run handler → return `CommandResult::Read`), and externally via a **logic-free, secret-free stdio shim** binary the user registers with their agent. Write tools **lower to existing dispatch arms**, so graph state still flows over the event stream; the MCP reply carries back only the minted ids (the lowered arms already return them inline — `cmd_node_agent_edit` → `{editNodeId,…}`, etc., so the shim needs no event subscription).

**Write-capable tools → additive Command mappings:**

| MCP tool | Lowers to | Inner capability (deny-by-default) |
|---|---|---|
| `list_node_types` | in-handler registry read | `NodesReadOutputs` |
| `create_agentic_node` (plan/ask/explore) | `NodeAgentRun{intent}` (new per-kind dispatch, §3a) | `ModelInvoke` (+`NetConnect`/`ProcessSpawn`) |
| `create_working_node` / `run_agentic_node` | `NodeAgentEdit` | `ModelInvoke + SnapshotWrite + ProcessSpawn + NetConnect` |
| `create_action_node` (non-snapshot kinds only) | `NodeCreate` | `SnapshotWrite` |
| `run_action_node` | `NodeRunCheck` | `SnapshotRead` (+`ProcessSpawn`) |
| `fork_from_node` | auto via fork-on-divergence; explicit `BranchFork` | inherits inner arm |
| `merge_node` | `BranchMerge` / `BranchMergeGated` | `SnapshotWrite` |
| `git_push_node` / `git_commit_node` | **new node-producing command** (R4) | `NetConnect` / `SnapshotWrite` |
| `restore_to_node` | `NodeRestore` | `SnapshotWrite` |
| `get_node_context` / `get_node_handoff` | `NodeContext` / `NodeHandoff` (P7 `LineageCompiler`) | `NodesReadOutputs` |
| `search_lineage` / `walk_ancestors` / `get_node_transcript` | delegate to the read `HistoryQuery` | `NodesReadOutputs` |

**Two correctness constraints (audit):** (1) **`NodeCreate` cannot mint snapshot-owning kinds** — it requires a `snapshot_hash` the external orchestrator can't compute (this is *why* `ProjectImport` exists). So `create_action_node` is scoped to **non-snapshot (Observing/Context) kinds**; a Working node comes into being only through `NodeAgentEdit`'s capture. State this in the tool contract. (2) **No new capability is minted** — the 7-set is frozen. Enforcement is **daemon-side** at each arm's `authorize()` (behind the core lock); the MCP only translates and propagates the denial. The desktop app exposes one checkbox — *"Allow an external orchestrator to create/run nodes."* Per-orchestrator scoping (e.g. "may edit but not push") is the real security surface and its additive home is the DESIGN §15.1 capability-scope grammar (D-8) — see OD-4.

**KV-cache goal:** `get_node_context` lets the external agent pull **only the lineage it needs** (the stable-`prefix_hash` `LineageCompiler` output), so an orchestrator can understand a node's history without ingesting the whole project — your goal (2).

**DoD (C1):** the pure `handle_request` ships with unit tests proving each write tool gates on its specific capability and refuses when ungranted (mirrors the read MCP's tests). Gating is not the deferred hard part.

---

## 5. Timeline-first UI reframe (renderer-side; + the hero chat + open-in-editor)

The mental shift: **nodes on emergent lanes are the hero; git framing is demoted to internal lane/fork bookkeeping.**

### 5a. De-git-ification (component by component)

| Component | Change |
|---|---|
| `TopBar.tsx` | **Remove** the branch switcher; replace with an informational **line breadcrumb** ("spork · main line"). Keep the model selector. Replace the node-verb toolbar with a single **"+ New node from here ▾"** menu (Agentic / Action groups). |
| `Navigator.tsx` | **Remove** the `BRANCHES` section; add a **LINES** overview derived from `groupBy(nodes, n=>n.branchId)` (label, tip node, count, roll-up state). Clicking a lane **focuses** it (never switches HEAD). No "+ new branch". Keep the type legend/filter. |
| `Canvas.tsx` + `layout.ts` | Add **lane swimlanes** (band + gutter label per `branchId`); a fork visibly branches into a new lane. Keep solid(lineage)/dotted(attachment) edges. Per-card "Start a new line from here" hover affordance. |
| `NodeCard.tsx` | **Remove** the raw `branchId` chip; put the **per-type state badge** in its slot (lane membership is shown by position). |
| `descriptors.ts` | Add offline-mirror descriptors for the new agentic + action kinds (unknown-kind already degrades gracefully). |
| `Modals.tsx` / `ContextMenu.tsx` / `toolbar.ts` | **Remove** NewBranchModal + BranchMenu + the `newBranch` action; reframe Merge as "merge into another **line**" (pick a lane *tip node*); regroup verbs into Agentic-/Action-node creation menus. |
| `StatusStrip.tsx` | "branch:" → "line:", non-clickable. |
| `store.ts` | Remove the `newBranch` `AppModal` + `branch` `ContextMenu` variants (renderer-local, C2-safe); add `focusLane(branchId)`. |
| `view.rs` + `ipc/types.ts` | Mirror additive `Option` `NodeView` fields: `presentation_status`, `line_label`, `forked_from`. **Bump `GRAPH_VIEW_SCHEMA_VERSION` only — register NO migration** (`GraphView` is a transient projection rebuilt every read, never persisted; `cost`/`gate` were added the same way). |

### 5b. The hero chat surface (your "beautiful chat that creates nodes")

A **first-class chat** is a primary view alongside the canvas — the nicest way to drive *one line* — not a buried tab. Chat and canvas are **two views of the same node substrate**:
- The chat renders the selected line's conversational nodes as messages; the **composer picks an agentic mode** (Ask / Plan / Work) — that sets the node `kind` — and **Send creates a node**.
- Sending while an **older node is selected forks a new line** (fork-on-divergence). Selecting a node in the canvas focuses the chat on its line; sending in the chat lights up the new node in the canvas.
- Per-type state renders inline as the message streams (`thinking → awaiting_input → require_review`), with a one-click "open this turn as a node" to jump to the canvas/detail.
- Built with the `frontend-design` skill so it's genuinely polished, not utilitarian. This replaces the flat `ConversationTab`; Node-Details keeps a **Thread** tab (the ancestry chain of conversational nodes, each a clickable mini-card).

### 5c. Family-driven Node detail (your "dig deep")

`NodeDetails.tsx` tabs become **family-driven**:
- **Agentic node:** Instruction · Agent (`{model, provider, intent, skills, tools, mcp}`) · What-it-did · Changes · **Thread** · Lineage.
- **Action node:** **Output** (recorded shell — 🟡 until the `RUN_STDOUT` producer lands) · Result · Changes · Lineage.
- **Every node:** **Codebase** (tree at this node) · **Ancestry** (chat chain) · **Tree** (ancestry across all lines) · **+ New node from here** actions. Relabel "Branch" → "Line".

### 5d. Open-codebase → external editor (the IDE handoff)

Spork is **not** a code editor; "Open codebase" launches your real editor (additive, app-local — same pattern as the folder picker):
- A new Tauri command **`open_in_editor(path, editor?)`** spawns the configured editor (`code` / `cursor` / `subl` / `idea`, or the system default), gated like the other app-local commands.
- A **Settings → Editor** preference (auto-detect `code`/`cursor` on PATH; fall back to the OS default).
- **Live project** → opens Spork's project root (drift-capture then tracks your edits back into the timeline). **A specific node's state** → materialize that node's snapshot to a stable path first (reuse the checkout/restore path), then open. In-app Monaco stays **read-only diff** only.

---

## 6. Phased plan (docs-first, then build — strictly additive)

Each phase ends with the CLAUDE.md §16 ritual (re-tag 🟡→🟢, honesty matrix, `cargo test/clippy/fmt` + app green, update TODO).

- **R0 — Docs realignment (this doc + IDEA/DESIGN/UI_UX/plan edits). No code.** ← *you are here; review gate.*
- **R1 — Connectivity unblock** (smallest user-visible win): unwire `copilot-cli` from defaults, fix the `agent.rs` doc example, make the `cli` arm fail loud with guidance; confirm local-HTTP is the default. *(Ships the bug you hit; no new contracts.)*
- **R2 — Additive view fields + UI de-git-ification**: add `presentation_status`/`line_label`/`forked_from` `Option` fields + the kind-gated payload read; bump the view schema version (no migration); build the renderer reframe (§5a) + the hero chat (§5b) + family detail (§5c) + open-in-editor (§5d).
- **R3 — New node-type descriptors + dispatch + state producer**: register the agentic + action descriptors; add the new daemon dispatch that stamps per-kind `kind` + rich payload; for Working, ship a new `agent-work` descriptor (OD-3); add the agent-loop **producer** that emits `presentation_status` (turns the badge live, 🟡→🟢).
- **R4 — git-push/commit as real nodes**: a new append-only node-producing command + `GitPushRunner`/`GitCommitRunner` that emit `NodeCreated` and record `CommandResult::Git` output onto a `ResultEnvelope` (do **not** touch the frozen `GitPush`/`GitExport`).
- **R5 — Orchestration MCP**: the `spork-orchestrate` crate + `OrchestrateInvoke` (+ optional `NodeSetPresentation`) command + the stdio shim binary + capability-gating DoD tests + the registration one-liner / a `spork-node` SKILL doc; pair the "allow external orchestrator" checkbox.
- **R6 — TLS BYOK transport**: a new `TlsHttpTransport` impl + the TLS dep + a `vaultRef` on `AgentConfig` (capable cloud models for Need A).

**Recommended order (OD-5):** R1 + R2 first (quickest user-visible wins — the connectivity bug + the de-git-ified, beautiful-chat UI), then R3/R5 (the substance: node types + the platform inversion), then R4/R6.

---

## 7. Frozen-contract (C2) risk register

Every place a naïve implementation would domino, with the additive escape hatch (audit corrections folded in):

| # | Risk | Additive escape hatch |
|---|---|---|
| R1 | "Reuse `NodeAgentRun` passing `mode`" — impossible on the wire (`intent: AgentRunIntent`; `cmd_node_agent_run` hardcodes `kind="agent-context"`). | New daemon dispatch maps per-kind agentic types onto the existing `intent`, stamping a distinct `kind` + richer payload. **New additive code, not a free reuse.** |
| R2 | "Working = a `mode` in `EditPayload`" — `EditPayload` is a **closed typed struct** (`to_value()` drops unknown keys). | New `agent-work` descriptor with its own payload schema (OD-3), **or** `EditPayload` v2 + a registered C5 migration (the migration registry is empty today). |
| R3 | Rich states colliding with frozen `Lifecycle`. | `presentation_status` in the node's **own payload**, surfaced via additive `NodeView.presentation_status` read by `get_payload` (not an envelope field). Total rich→Lifecycle map; `require_review → Blocked`. |
| R4 | `presentation_status` has **no producer** — vapor until the agent loop emits it. | Ship forward-mapped (🟡, `None`-default) + a scoped producer (R3). Don't advertise live values prematurely. |
| R5 | `NodeCreate` can't mint snapshot-owning kinds (needs a content hash the orchestrator can't compute). | Scope `create_action_node` to non-snapshot kinds; Working only via `NodeAgentEdit` capture. |
| R6 | New `Command` variants must not reorder/edit existing ones. | Append-only; `#[serde(tag="command")]` makes the wire tag a stable string — same move as `HistoryQuery`/`ProjectImport`/`NodeAgentEdit`. |
| R7 | **git-push/commit are NOT nodes for free** — `GitPush`/`GitExport` emit **no `OpLogEvent`**. | New node-producing command + runner that emits `NodeCreated`. **Never add event emission to the frozen `GitPush`/`GitExport`** — that edits their frozen "emits no event" contract. |
| R8 | "Set node state by appending a child" — appending a child does not change the parent's rendered state. | Carry `presentation_status` in the **target node's own payload** + the view read. Optional `NodeSetPresentation` writes the target's payload; do **not** fan out child micro-nodes. |
| R9 | Capability minting / enforcement misplacement. | Reuse the frozen 7. **Enforcement is daemon-side** at each arm's `authorize()`; the MCP only propagates the error. Per-orchestrator scoping → DESIGN §15.1 grammar (D-8). |
| R10 | TLS transport claimed "shipping" — only `http.rs`/`subprocess.rs` exist. | New `Transport` impl behind the frozen seam + a TLS dep — real new work (R6). |
| R11 | `EdgeType` / new action edges. | `EdgeType` (in `spork-edges`) frozen set untouched; new action kinds via `NodeRunCheck` inherit the right edge via `observing_edge_for` (verify a new kind doesn't fall to the `_ => Checks` default unintentionally). Verify `agent-work`'s `allowed_edges` is a superset of what the edit path emits (`DisallowedEdgeType` is enforced). |
| R12 | "Register a migration" for new view fields. | **No migration** — `GraphView` is a transient projection, never persisted. Add `Option` fields + bump `GRAPH_VIEW_SCHEMA_VERSION` only. The D-4 registry is for *persisted* schemas. |
| R13 | Renderer-local `newBranch`/`branch` removals. | Subtractive on renderer types only; the IPC `Command` union is untouched. |
| R14 | Paired-UI rule (CLAUDE.md §8) for the new MCP surface. | UI_UX §14 + honesty matrix gain the Orchestration MCP surface and re-tag the deprecated CLI-as-model route — done in R0/R5. |

---

## 8. Open decisions (recommendations adopted in this doc — flag to override)

1. **OD-1 — one axis, not two.** The agentic *type* (`kind`) carries identity and maps onto the existing `intent {Ask, Plan, Analysis, Change}` (`Exploratory → Analysis`), rather than a parallel `mode` taxonomy. **Adopted: yes.**
2. **OD-2 — `require_review` gates.** `require_review → Blocked` (dependents stall until a human acts). **Adopted: yes** (the safe, engine-honest choice; override only with an explicit "review never gates" decision).
3. **OD-3 — Working carrier.** A new standalone `agent-work` descriptor (separate payload schema, cleaner legend) over an `EditPayload` v2 migration. **Adopted: new descriptor.**
4. **OD-4 — orchestrator scoping.** Start with one global "allow external orchestrator" checkbox; per-client scopes ("may edit but not push") land later via the §15.1 scope grammar. **Adopted: global for now** — confirm, since it's the real security surface.
5. **OD-5 — order.** Ship **R1 + R2** first (connectivity unblock + de-git-ified beautiful UI), then R3/R5, then R4/R6. **Adopted: yes.**

---

## 9. Doc-update checklist (R0) — ✅ done

- [x] **REALIGNMENT_PLAN.md** (this file) — the authoritative realignment.
- [x] **IDEA.md** — realignment note + reframed non-goals (not-a-code-editor / not-a-model-host) + D2 (two node-type families) + D5 (platform inversion via MCP).
- [x] **DESIGN.md** — a prominent realignment note up top pointing here + the contract-level framing (branches-emergent, the two families, presentation-status + `require_review→Blocked`, the Orchestration MCP, connectivity routes, generic-CLI deprecation, open-in-editor).
- [x] **UI_UX_DESIGN.md** — the realignment surfaces (LINES, hero chat, family tabs, state badges, open-in-editor, Orchestration MCP — all 🟡) + the command-list note.
- [x] **IMPLEMENTATION_PLAN.md / TODO.md** — a Realignment (R0–R6) section/pointer; TODO carries the live R0–R6 checklist + adopted ODs.
- [x] **CLAUDE.md** — docs-table row + the positioning paragraph.
