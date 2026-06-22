# Spork — The Idea

> **Purpose of this file.** A short, durable capture of *what Spork is and why it exists*, so the original intent is never lost as the code and the other docs grow. It is deliberately non-technical and stable. For the authoritative spec see [DESIGN.md](DESIGN.md); for what gets built when, see [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) and [TODO.md](TODO.md); for how it should look and feel, see [UI_UX_DESIGN.md](UI_UX_DESIGN.md).
>
> **Realignment note (2026-06).** Running the first MVP showed the UI had drifted *git-ward* (a branch sidebar, a branch switcher) and buried the spine — the **timeline of typed nodes**. The **timeline-first / node-centric / platform-inverted** refinement in [REALIGNMENT_PLAN.md](REALIGNMENT_PLAN.md) sharpens this vision; it does **not** change the bet. In one line: **Spork is the branching-timeline + sandbox + agent-orchestration layer that sits *on top of* the agents and editors you already use** — it connects to your agents over **MCP**, delegates editing to your **IDE**, and is not a git tool, not a code editor, and not a model host.

---

## 1. One sentence

**Spork is a free, open-source agent-centric IDE built around a persistent, project-level branching timeline — a DAG — of *typed work nodes*, where every node carries a content-addressed, restorable snapshot of the whole codebase.**

## 2. The elevator pitch

Today, working with coding agents means trusting a fragile, linear stack of checkpoints. You edit, the agent edits, a test runs in a terminal, you revert — and history is a throwaway line you can't branch, can't trust, and can't inspect. Spork replaces that line with a **graph**. Every change by a human or an agent becomes a durable, typed node you can click to see the *exact* working tree, restore non-destructively, or branch from to explore an alternative. Tests, stress runs, and sanity checks stop being ephemeral terminal output and become first-class nodes hanging off the work they observe. Models from any provider are peers you switch between per node, with lineage-aware context compiled for you.

The aim is **workflow and trust, not market capture.** Spork is free and open-source; the only bar is whether the workflow is good enough that people adopt it.

## 3. Why it exists — the linear-history failure mode

Every mainstream agent tool today models history as a *stack*, and it fails four ways. Spork is the fix for all four:

| # | Failure in today's tools | What Spork does instead |
|---|---|---|
| **1. Destroyed forward history** | Revert collapses the timeline; a discarded exploration is gone, and you can't compare two ideas side by side. Some tools' "revert" is itself irreversible. | **Restore is an *event*, not an overwrite.** Forward history survives as a branch. Two ideas live as two branches you can diff. |
| **2. Untracked mutations** | `bash rm/mv`, codegen, and build steps escape edit-tool tracking, so a node's recorded state silently diverges from disk — the single biggest reliability gap in every competitor. | **Three-source drift capture** fuses an interceptor, a filesystem watcher, and a reconciliation rescan, so *no* change goes unattributed. The recorded state is the real state. |
| **3. Chat/code divergence** | Snapshots restore files *or* chat by timestamp, so a restored agent acts against state it doesn't understand. (Research: chat-only recovery ≈ 8–13% correctness vs ≈100% for code+conversation together.) | **Transactional dual-restore:** code, conversation, and the op-log pointer move together, atomically, failing closed. |
| **4. Ephemeral observations** | Test/lint/perf output lives in a terminal — not durable, not diffable across branches, not usable as a gate. | **Checks are durable graph citizens:** typed observing nodes carry results that diff across branches and can gate transitions. |

## 4. The defining bet (D1 + D2 + D3)

The bet is that the central experience should be a **typed, user-extensible work-DAG where every node is a restorable, content-addressed sandbox, and check nodes auto-run after edits.**

An adversarial prior-art sweep (refuters tasked with proving it already exists) found that the *pieces* exist somewhere — restore (GitButler, Cline, Replit), the graph shape (git GUIs, jj, Sapling), multi-provider switching (Aider, Continue) — but they **never fuse**. The genuinely empty quadrant across the whole landscape is **D2: heterogeneous typed work nodes** (edit vs. validation vs. stress vs. deterministic sanity-check) as first-class, diffable, restorable graph citizens, plus a registry that lets users define new types. Every shipping timeline is made of *homogeneous* nodes — commits, operations, generic checkpoints. The closest counter-example (Zed's DeltaDB) reaches ~4–5 of 6 but is a homogeneous edit-stream with no typed nodes. It's unbuilt because the contribution is *integration-shaped* — fusing D1+D2 onto an already-commoditized D3/D4/D5 substrate — and nobody has made typed, auto-running, restorable check nodes the spine of an IDE.

## 5. The six dimensions

| | Dimension | In one line |
|---|---|---|
| **D1** | Project-level branching work-DAG | A persistent, content-addressed graph of typed lifecycle nodes — not chat history, not file-version history — as the single source of truth and primary surface. |
| **D2** | Typed node types (+ user-definable) | First-class heterogeneous nodes in two families: **agentic** (Planning / Ask / Exploratory / Working — each an agent "mode" differing by its instructions, skills, tools, and MCP servers) and **deterministic action** (run-tests, stress, sanity, git-push/commit — repetitive actions as nodes recording shell output + result), plus code-edit/snapshot/merge — all through one registry users extend with the same contract the built-ins use. Every chat turn and every action is a node. |
| **D3** | Per-node content-addressed restorable sandbox | Click any node to resolve its exact working tree; restore or branch from anywhere non-destructively, with every human and agent change attributed to a node. |
| **D4** | Local-first, expandable to shared-team | Single-machine and private by default, expanding to teams by grafting content-addressed node+sandbox bundles onto one shared project DAG — the same machinery at wider scope, not a separate distributed system. |
| **D5** | Multi-provider + platform inversion | Two connectivity arrows: **Spork drives a model** for its own nodes (local/Ollama by default, cloud BYOK over TLS) — model choice per node with cost attribution; and **your agent drives Spork** — Claude Code / Copilot / Cursor connect over a write-capable **Orchestration MCP** (the "Spork Node skill") and create/run Spork nodes themselves. Agent CLIs are *not* impersonated as one-shot models (see [REALIGNMENT_PLAN.md](REALIGNMENT_PLAN.md) §2). |
| **D6** | Lineage-aware managed context + handoff docs | Context is compiled from DAG lineage with cache-friendly ordering, and parent→child handoff documents are auto-generated so a fresh agent can start cold. |

## 6. The mental model in one diagram

```
                      ┌──────────────┐   validates    ┌─────────────────┐
                      │  Edit  ✎     │◀───────────────│  Validation  ✓  │   (observing:
              ┌──────▶│  owns snapshot│                │  pass/fail       │    no snapshot,
              │       └──────┬───────┘                 └─────────────────┘    attaches results)
   ┌──────────┴───┐         │ branch / fork
   │  Snapshot ▣  │         ├───────────────▶┌──────────────┐   checks   ┌─────────────────┐
   │  origin=import│        │                 │  Edit  ✎     │◀───────────│  Sanity  ◎      │  (auto-runs
   └──────────────┘         │                 └──────┬───────┘            │  violations[]   │   after edits)
       (mutating:           │ alternative line       │                    └─────────────────┘
        owns snapshot)      ▼                         ▼ merge
                     ┌──────────────┐          ┌──────────────┐
                     │  Edit  ✎     │─────────▶│  Merge  ⤳    │  (3-way; only created once
                     └──────────────┘          └──────────────┘   every conflict is resolved)

 Click any node → its exact working tree (lazy, O(changed files)).   Restore = an event, not an overwrite.
 Mutating nodes own a snapshot.  Observing nodes attach results.  Context nodes (Plan/Conversation) carry neither.
```

## 7. Core vocabulary (learn these six)

- **Node** — a typed work unit. One *Envelope* (identity, type, parents, snapshot pointer, status, model/cost) plus a type-specific *payload*. In one of three families: **mutating** (owns a snapshot), **observing** (attaches results), or **context** (no snapshot).
- **Edge** — a typed, directional relation: `PARENT_CHILD` (lineage), `BRANCH` (fork origin), `DERIVED_FROM` (provenance), `VALIDATES`/`CHECKS`/`STRESSES` (observation), `MERGE_PARENT`.
- **Sandbox / Snapshot** — the content-addressed, restorable whole-codebase state bound to a node. Dedup-by-hash (Git-style blob/tree/snapshot): unchanged files cost **zero** new bytes, and clicking a node resolves its exact tree.
- **Branch** — a parallel line of work forking from any node. Branching and restore are O(diff), not O(repo). **Branching is automatic, not a chore:** starting code-changing work *continues* the current branch when you're at its tip, but **auto-forks a new branch** the moment you start from an earlier/already-extended node — so an existing line can never be silently overwritten. Read-only work (analysis, planning) *attaches* as a node on the current line rather than forking (it owns no snapshot). Lineage connections draw solid; attachments/observations draw dotted, so it's always clear which branch a node belongs to. (Policy is daemon-owned; the agent-run and historical-checkout triggers arrive in later phases.)
- **Handoff (document)** — an auto-generated, regenerable `AGENTS.md`-style distillation of parent→child lineage that lets a fresh agent start cold.
- **Drift** — out-of-band, untracked working-tree mutation (bash `rm`/`mv`, codegen, external edits) caught by three-source capture and reconciled into a Snapshot node so nothing goes unattributed.

## 8. Who it's for

- **Primary beachhead:** AI-native power users running 2–4 agents in parallel who already feel the pain and bolt on third-party tools (ccundo, Conductor, Vibe Kanban) to approximate it.
- **Secondary:** regulated/privacy-sensitive teams (local-first + BYOK + Ollama is structurally inaccessible to cloud incumbents), and jj/Jujutsu and VCS enthusiasts with an appetite for a better content-addressed history model.
- **Explicitly low-priority at first:** mainstream VS Code/Cursor users — they need a 10× payoff to adopt a new mental model, so Spork should be *additive* (CLI/daemon + MCP, or an extension) for them rather than a replacement IDE.

## 9. What Spork is *not* (non-goals)

- **Not** a commercial land-grab. Free and open-source; adoption, not revenue, is the bar.
- **Not** a Git replacement. It is git-aware and interoperates via a `.spork/` sidecar, never polluting real history. A "branch" is an **emergent line** in the node DAG, never git chrome the user manages; `branch`/`HEAD`/`refs` survive only as internal tip/fork bookkeeping. Git push/commit are **action nodes**, not primary verbs (see [REALIGNMENT_PLAN.md](REALIGNMENT_PLAN.md)).
- **Not** a code editor. "Open codebase" hands off to your real IDE (VS Code / Cursor) via open-in-editor, like Claude Desktop and the Copilot app; in-app Monaco is read-only diff viewing only. Spork is *additive* to the editor you already use.
- **Not** a model host or an agent runtime. Spork drives a *model* only for its own in-app nodes (local HTTP, or BYOK over TLS); your **existing agents drive Spork** through a write-capable Orchestration MCP (the "Spork Node skill"). Agent CLIs are not impersonated as one-shot models.
- **Not** execution-replay time-travel (rr/Pernosco-style, 10–20× overhead). Spork does *cheap snapshot* time-travel, not deterministic replay.
- **Not** a bet on indexing/model breadth. Model connectivity is a thin, swappable adapter.
- **Not** a promise that every branch runs in parallel. Worktrees share DBs/ports/Docker and storage balloons; the scheduler **serializes honestly** when resources conflict.
- **Not** frozen-RAG/vector search over code. Retrieval is a budgeted tool the agent calls.
- **Not** live co-editing / cloud CRDT multiplayer in v1. Async shared-team (bundle grafting) is the resolved model; only *live* co-editing is deferred.
- **Not** a claim to undo external side effects. Shared-DB writes, pushed remotes, and paid-API spend are **surfaced, not silently undone.**

## 10. The product feel (north star)

Spork should feel like **a map of your work that you trust completely.** Quiet, dense-but-legible, fast at thousands of nodes, and *honest* — it never implies fidelity it doesn't have (uncaptured drift is flagged; irreversible effects are warned; a slow checkout is labelled slow). The graph is the hero; everything else gets out of its way. See [UI_UX_DESIGN.md](UI_UX_DESIGN.md) for how that translates into pixels.
