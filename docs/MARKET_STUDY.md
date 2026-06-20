# Spork — Agent-Centric IDE
## Market & Adoption Assessment (Open-Source)

| | |
|---|---|
| **Working codename** | Spork |
| **Document** | Market & Adoption Assessment (prior-art + adoption decision memo) |
| **Status** | Draft for review |
| **Version** | 0.2 (reframed for an open-source, non-commercial goal) |
| **Date** | 2026-06-20 |

> **Why this document exists.** Spork is intended as a **free, open-source** product — market capture is *not* a goal. This study therefore answers only the two questions that actually gate the decision to build:
> 1. **Does this already exist?** Is there a product that already delivers Spork's core combination, in which case building it would be wasted effort?
> 2. **Will it be adopted?** If built and given away, would developers actually use it?
>
> Commercial sizing (TAM/SAM/SOM), pricing tiers, and revenue go-to-market — the contents of v0.1 — have been deliberately removed as out of scope. What remains is a prior-art verdict, an adoption outlook, and a build/no-build recommendation.

---

## Table of Contents

1. Purpose & Verdict (Decision Summary)
2. The Question: What "This" Means
3. Does This Already Exist? — Prior-Art Sweep
4. Closest Analogs & What They Lack
5. Feature / Overlap Matrix
6. Adoption Outlook for an Open-Source Product
7. Who Adopts & Why (Segments & Wedge)
8. What Would Make It Spread — and What Would Kill It
9. Recommendation
- Appendix A — Methodology, Sources & Confidence

---

## 1. Purpose & Verdict (Decision Summary)

This is a build/no-build memo, not a marketing case. The analysis behind it was an adversarial one: five prior-art sweeps across the entire tool landscape, **three independent agents tasked specifically with trying to *prove the idea already exists*** (a strict-reading refuter, an "you can just assemble it from parts" refuter, and an adjacent-space hunter), and three calibrated adoption angles. The refuters could not find a full match. The verdict is therefore stated with reasonable confidence.

### 1.1 The bottom line

| Question | Verdict | Confidence |
|---|---|---|
| **Does this already exist?** | **Partially.** The *integrated whole* — a persistent, project-level, **branching DAG of TYPED work nodes** where every node carries a **restorable, content-addressed sandbox**, used as the **primary IDE surface** — is **not shipped by anyone**. Every competitor delivers a *piece*: a linear checkpoint stack, a homogeneous commit/operation DAG, an ephemeral per-run task tree, or a parallel-agent kanban — never the fusion. | High |
| **Will it be adopted (as OSS)?** | **Moderate**, swinging to **high** if delivered *additively* (CLI/daemon + MCP, or a VS Code extension) rather than as a replacement IDE. The underlying pains are validated by revealed preference, not just sentiment. | Medium-High |
| **Should you build it?** | **BUILD — BUT NARROW.** The concept clears the novelty and demand bars. The risk is *form factor and timing*, not the idea. Narrow the wedge and the packaging; do **not** build the maximalist from-scratch IDE first. | — |

### 1.2 In one paragraph

The thing you described is real whitespace. After a deliberate attempt to debunk it, no shipped product — and no waitlisted one — combines D1+D2+D3 (typed work-DAG + per-node restorable sandbox, as the central experience). The closest shipped product, **GitButler**, reaches ~3 of 6 dimensions on a *linear, untyped* operations log; the strongest single counter-example, **Zed's DeltaDB** (waitlisted, June 2026), reaches ~4–5 of 6 but is a homogeneous CRDT edit-stream with **no typed nodes (D2)** — and D2 is the *genuinely empty quadrant across the entire landscape*. The demand is real: "never lose agent work" already spawned a third-party patch ecosystem and went native everywhere, and parallel-branch experimentation is 2026's hottest tooling frontier. The catch is that most *individual* dimensions are now table-stakes and free, so the **bundle and the integration must carry the product**, and a standalone editor forfeits the marketplace distribution that carried every recent OSS winner. Hence: build the engine, ship it where developers already are, and lead with the one workflow nobody else has.

---

## 2. The Question: What "This" Means

"Does this exist?" is only answerable against a precise definition. Spork is scored against six defining dimensions; novelty is judged on the **combination**, especially the **D1+D2+D3 core** as the *central* experience (not as a buried feature).

| Dim | Capability | How mature is it *in isolation*? |
|---|---|---|
| **D1** | Persistent **project-level branching timeline (DAG)** of typed *work* nodes — not chat history, not file-version history | The *graph shape* is ubiquitous (every Git GUI, jj, Sapling). A **work**-DAG used as the primary surface is rare. |
| **D2** | Heterogeneous **typed node types** — agent-edit, validation/test, stress-test, deterministic auto-running sanity/pattern-check — **plus a user-definable node-type registry** | **Essentially absent everywhere.** The empty quadrant. Closest: Antigravity "Artifacts", CI gates — but those are ephemeral, not durable restorable graph citizens. |
| **D3** | **Per-node sandboxed, content-addressed, restorable** codebase state: click any node → exact working tree; restore/branch from anywhere; *all* human + agent changes attributed to a node | The **most-solved** dimension (GitButler oplog, Replit App History, Cline shadow-git). But always *linear* or *whole-project*, not per-typed-node. |
| **D4** | **Local-first**, expanding to shared-team by **sharing sandbox states** on the same project DAG, reconciled in one view | Partially in DeltaDB (CRDT worktrees). Everyone else reconciles via git merge/PR, not shared restorable sandbox nodes. |
| **D5** | **Multi-provider** agent with fast switching (Claude / OpenAI / Copilot CLI / Ollama) | **Commoditized.** Aider/LiteLLM (100+), Goose, Kilo (500+), GitButler, Continue. Not a differentiator. |
| **D6** | **Lineage-aware managed context** + automatic parent-child **handoff documents** | Emerging (Amp retired compaction for handoff). Nobody ties it to a typed, restorable DAG. |

### 2.1 The novelty test

A product "already exists" only if a **single** product delivers **D1 + D2 + D3 together as its primary UX**. Individual dimensions failing this test (D3 restore, D4 isolation, D5 switching, D6 context) are explicitly treated as *commodity prior art* and do not count against novelty — they count against *differentiation*, which is why the recommendation (§9) leans so hard on D2 and on integration.



## 3. Does This Already Exist? — Prior-Art Sweep

The short answer is **partially, but not as an integrated whole**. Every individual primitive in Spork's design ships somewhere today, and a determined developer can hand-assemble most of the experience. But no single product delivers the defining combination — and specifically not the **D1+D2+D3 core** (a persistent, project-level, branching DAG of *typed* work nodes, each carrying a content-addressed, restorable sandbox) — as its central experience. Confidence on this negative finding is **high**, qualified by the speed of the category: searches spanned four prior-art families plus three adversarial passes (assembly lens, infra lens, June-2026-entrant lens).

### 3.1 Where the pieces already live

The market has independently matured nearly every ingredient, which is why Spork "sounds" already-built:

- **Per-node / per-checkpoint restorable state (D3-flavored)** is the most thoroughly solved dimension anywhere. GitButler's oplog snapshots the full working directory before every operation with one-click revert (high confidence). Replit's snapshot engine restores code **and** database (Neon copy-on-write branches) **and** workspace **and** conversation context together — the broadest restore scope in any product (high confidence). Devin rolls back files **and** agent memory within a session via ~200ms blockdiff VM snapshots (high confidence). CodeSandbox forks/restores any snapshot in under 2s (high confidence).
- **A branching-graph *visualization* (D1's graph shape)** is the entire premise of the VCS-tooling category: Git GUIs (GitKraken, lazygit, GitLens Commit Graph), Jujutsu (jj), and Sapling all render commit/operation DAGs natively (high confidence).
- **Multi-provider fast switching (D5)** is effectively commoditized: Aider/Continue via LiteLLM (100+ providers), Kilo (500+ models), Goose (25+), Zed via the open Agent Client Protocol, GitButler (OpenAI/Anthropic/Ollama/LM Studio), all high confidence.
- **Parallel-agent isolation via git worktrees (D3/D4-adjacent)** became standard across the category in Q1–Q2 2026 — Cursor, Factory, Augment, Jules, Conductor, Nimbalyst, Agentastic, Verdent (high confidence).
- **Managed/lineage-aware context (D6)** exists in pieces: Augment's Context Engine + Memories (semantic retrieval over 400k+ files) and Cognition's handoff documents (high confidence).

### 3.2 The concept is not novel; the productization is

The underlying *pattern* — typed nodes carrying checkpointed, serializable, resumable state with branching edges — is mature in the agent-framework layer. LangGraph explicitly models "the loop as a graph, every step a node, state typed and checkpointed, pause at any node and serialize" (medium confidence on framing). ML-pipeline tooling proves a typed, content-addressed, reproducible DAG-of-work is shippable: DVC declares typed pipeline stages and caches runs by content hash (high confidence). So Spork invents no new primitive; its contribution is *productizing* the pattern as a local-first, human-facing IDE for codebase editing.

### 3.3 The genuinely empty quadrant: D2

Across all four prior-art categories and three adversarial passes, the consistently **unbuilt** element is **D2 in full**: heterogeneous *typed* work nodes (agent-edit vs. validation vs. stress-test vs. deterministic auto-running sanity/pattern-check) as first-class, diffable, restorable graph citizens, **plus a user-definable node-type registry**. Every shipping timeline is made of *homogeneous* nodes — commits (Git GUIs, jj, Sapling), operations (GitButler oplog), or generic checkpoints (Cline, Claude Code, Replit). The closest approximations are Google Antigravity's typed "Artifacts" (but these are verification traces/approval surfaces, not restorable branch points) and Bernstein's CI-style lint/type/test gates (but these are ephemeral pass/fail gates over throwaway worktrees, not durable nodes on a restorable graph). No shipped product makes deterministic check **nodes** that auto-run after edits and persist as comparable, gate-able artifacts. Confidence: **medium-high** on this gap being real and consistent across the landscape.

## 4. Closest Analogs & What They Lack

Below are the products that land nearest to Spork's central experience, ordered by how close they get. The recurring shape is instructive: products either have **D1's graph shape but untyped, non-restorable nodes** (VCS tools), or **restorable state but a linear, untyped timeline** (agent checkpointers) — never the fusion.

### 4.1 Zed DeltaDB — the strongest single counter-example (~4–5 of 6; medium-high confidence)

A surfaced June-2026 entrant (waitlist stage) and the closest threat to Spork's novelty. DeltaDB is an operation-based/CRDT version-control substrate where *any point in history is a valid branch point, including mid-run*, the worktree is virtualized so branching is near-free (strong **D1**), you can mount any past worktree to disk with per-operation addressable identity (strong **D3**), conflict-free replicated worktrees let humans **and** agents edit the same files concurrently across machines (strong **D4**, arguably exceeding Spork), and agent conversations are stored side-by-side with the edits they produced, bidirectionally traceable (partial **D6**). The surrounding Zed editor adds **D5** via ACP.

**What it lacks:** **D2 is the decisive miss.** Nodes are homogeneous edit/operation deltas, not typed work units; DeltaDB explicitly leaves checks to "Git and CI" — there are no auto-running deterministic check nodes and no user-definable node-type registry. It is a VCS layer *beneath* an editor, not a typed work-DAG *as* the primary IDE surface. No automatic parent-child handoff documents. This is the competitor to watch most closely: if it adds typed/checkable nodes, the remaining D2 novelty could be commoditized.

### 4.2 GitButler — closest *shipped* integrated product (~3 of 6; high confidence)

The only shipping product combining three things at once: each AI session on its own branch with automatic per-prompt commits (proto-D2/D3 attribution); a project-level operations timeline with click-to-revert on any entry (genuine **D3** restore + timeline view); and native multi-provider selection (real **D5**). Raised a $17M Series A in 2025 (medium confidence).

**What it lacks:** the timeline is a **linear operations log of untyped snapshots** (OperationType is an internal enum), not a branching DAG of user-meaningful typed nodes — **D1** only partial. **D2 entirely absent.** Restore is whole-project undo, not per-typed-node sandboxes as the branchable unit. Sharing remains git/PR-shaped (no shared sandbox states on one DAG). No lineage-aware ancestor-context selection or auto handoff docs.

### 4.3 Replit Agent / App History — best-in-class restore breadth (~2 of 6; high confidence)

Exceeds Spork on environment restore *scope*: code + DB + workspace + conversation, with bidirectional navigation. **What it lacks:** explicitly a **linear** unified timeline, not a branching DAG (**D1** largely absent); checkpoints are generic/untyped (**D2** absent); cloud-hosted with ~7-day retention (opposite of local-first persistent history); single-vendor agent (**D5** absent); greenfield-app oriented rather than a navigable graph over an existing codebase.

### 4.4 Plandex — closest in the OSS/CLI category (~2.5 of 6; high confidence)

The only open-source terminal agent combining real branching version control (branch/rewind/diff plan states, branches to compare models) with a sandbox isolating AI changes until approved, plus full **D5**. **What it lacks:** branching is **per-plan with linear history inside a branch**, not a persistent project-level multi-parent DAG; nodes are plan/diff states, not typed work nodes (**D2** unmet); the sandbox is a cumulative diff-review staging buffer, not a content-addressed per-node working tree every change is attributed to; no **D4**, no **D6**.

### 4.5 iloom — closest emerging tool with a real DAG over real code (~2 of 6; high confidence)

The only emerging tool combining an actual DAG with parallel coding agents on a real codebase: a dependency DAG of issues in VS Code, parallel Claude agents each in isolated worktrees respecting the DAG, child looms inheriting parent branch + DB state (lineage-like). **What it lacks:** the DAG is a **planning/dependency graph of issues**, not a timeline of executed typed work nodes; worktrees are **ephemeral** (cleaned on finish) with no restore-any-point-and-branch; no typed check nodes; Claude-only (fails **D5**); no sandbox-state sharing as the reconciliation unit (**D4**).

### 4.6 Cline + worktree orchestrators — closest shipped per-node-restore mechanism (~2 of 6; high confidence)

Cline's shadow-git checkpoints (commit after each tool use, three restore modes, persist across sessions, ~62K stars, 5M+ installs) are the most widely-adopted restore mechanism; orchestrators (Conductor, Vibe Kanban ~27K stars, Nimbalyst) add parallel-agent isolation. **What it lacks:** checkpoints are **linear within a single chat task**, not a branching DAG; untyped snapshots, no node types or auto-running checks; orchestrators explicitly lack a persistent branching timeline and per-node rollback; reconciliation is plain git merge.

### 4.7 The assembly argument and why it falls short (high confidence)

A developer *has* hand-assembled the nearest approximation — the publicized "local autonomous GitHub with jj workspaces" setup (jj workspaces + Cursor CLI + terminal tabs + dev servers) whose agents autonomously merge each other's work, a real stab at D1+D3+D4. But the assembled whole degrades on exactly the defining dimensions: none of the timelines is a *persistent, typed* work-DAG (jj/GitButler render homogeneous commit/op graphs; Bernstein builds an *ephemeral per-run* task tree); the parts share no data model, so "click any node, see the exact tree, branch with lineage-aware context and auto-handoff docs" never emerges; and D2, D4, and D6 would have to be built from scratch.

## 5. Feature / Overlap Matrix

Scoring the closest analogs against Spork's six dimensions. Marks: ● = filled (genuinely delivered as a central capability), ◐ = partial (present but limited, indirect, or non-central), ○ = empty (absent). Overlap count sums filled (1.0) and partial (0.5) marks. All scores are calibrated against the dimension as Spork defines it, not a looser reading; confidence is high except where noted.

| Product | D1 Typed work-DAG | D2 Typed/auto-run node types | D3 Per-node restorable sandbox | D4 Sandbox-state sharing | D5 Multi-provider switch | D6 Lineage-aware context | Overlap (/6) |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| **Zed DeltaDB** (waitlist; med-high conf.) | ◐ | ○ | ● | ● | ◐ | ◐ | **3.5** |
| **GitButler** | ◐ | ○ | ◐ | ◐ | ● | ◐ | **3.0** |
| **Plandex** | ◐ | ○ | ◐ | ○ | ● | ○ | **2.5** |
| **Devin / Cognition** | ○ | ○ | ◐ | ○ | ○ | ◐ | **1.0** |
| **Replit Agent / App History** | ◐ | ○ | ● | ○ | ○ | ◐ | **2.0** |
| **iloom** | ◐ | ○ | ◐ | ○ | ○ | ◐ | **1.5** |
| **Cline** | ○ | ○ | ◐ | ○ | ● | ○ | **1.5** |
| **Google Antigravity** | ○ | ◐ | ◐ | ◐ | ◐ | ○ | **2.0** |
| **Cursor (2.0/3.0)** | ○ | ○ | ◐ | ◐ | ● | ○ | **2.0** |
| **Zed (Agent Panel + ACP)** | ○ | ○ | ◐ | ◐ | ● | ◐ | **2.5** |
| **Augment Code** | ○ | ○ | ◐ | ◐ | ◐ | ● | **2.5** |
| **Factory AI (Droid)** | ○ | ◐ | ○ | ◐ | ◐ | ○ | **1.5** |
| **DVC** (adjacent: data-DAG) | ◐ | ○ | ◐ | ◐ | ○ | ○ | **1.5** |

### 5.1 Honest reading of the matrix

Three things stand out, and the study should not soft-pedal them:

1. **Competitors already overlap meaningfully — typically 2–3.5 of 6.** This is not a greenfield. D3 (restore), D4 (worktree isolation), and D5 (multi-provider) are commoditizing fast; treating any one of them as the differentiator would be a mistake.
2. **The D2 column is almost entirely empty.** Only Antigravity and Factory earn even a partial mark, and both for *typed agent outputs / task decomposition* rather than restorable, auto-running, user-definable check **nodes** on a graph. D2 — and specifically auto-running deterministic check nodes plus a user-definable node-type registry — is the genuinely empty quadrant across the entire landscape.
3. **No row scores high on D1 *and* D2 *and* D3 together.** Zed DeltaDB (3.5) is strong on D1/D3/D4 but empty on D2; GitButler (3.0) is strong on D3/D5 but its D1 is a linear untyped log. The integration — a *typed, user-extensible work-DAG where every node is a restorable content-addressed sandbox and check nodes auto-run* — is delivered by no product and does not spontaneously emerge from the strongest available hand-assembly.

The defensible novelty, therefore, is narrow and **integration-shaped**: Spork's contribution is fusing D1+D2 (the typed, user-definable work-DAG with auto-running validation nodes) onto the already-commoditized D3/D4/D5 substrate, as the central IDE surface — not the invention of any single dimension.


## 6. Adoption Outlook for an Open-Source Product

For a free, open-source tool, adoption is gated less by features than by three levers in order: distribution, install friction, and one undeniable workflow. On all three, the evidence for Spork is mixed-positive: the underlying *problem* is validated by revealed preference, but the proposed *form factor* (a standalone Tauri IDE leading with a graph UI) concentrates the risk.

### 6.1 The base rate from comparable OSS dev tools

The recent OSS winners cluster around the three levers, and Spork's standalone-IDE packaging forgoes the strongest one.

| Tool | Traction (confidence) | Won on | Spork parallel |
|---|---|---|---|
| Cline | ~62K stars, 5M+ VS Code installs (high) | VS Code marketplace + BYO-key | Forgoes marketplace |
| Aider | ~41–44K stars, 5.3M+ PyPI dl, ~15B tok/wk (med-high) | pip install, 100+ models, terminal niche | Match install bar |
| Continue | ~26K stars; pivoted to CLI/PR-review (med) | Marketplace + free-vs-Copilot | Cautionary: wedge churn |
| Cursor | $0→~$2B ARR, 1M+ users (high) | VS Code muscle-memory + 10x workflow | The bar as a fork |
| Zed | ~75K stars, $32M Sequoia (high) | Native perf, slow ramp | Standalone, gradual |
| Ollama | ~169K stars (high) | Single installer, local | Aligns w/ D4/D5 |

The pattern is unambiguous: every mass-adopted product either rode the VS Code marketplace (~75.9% editor share, Stack Overflow 2025) or shipped a one-command/single-binary install — usually both. Spork, as a non-extension IDE, inherits the harder path that only Cursor and Zed have walked, and Cursor cleared it only by preserving 100% VS Code compatibility plus one-click config import.

### 6.2 Net read: moderate, swinging on packaging

Demand for the core value props is real (Section 7), and the integrated D1+D2+D3 whole is genuinely unbuilt as of mid-2026 (verified against Cline checkpoints, worktree orchestrators, Replit, GitButler, and Zed's DeltaDB). But most individual dimensions — checkpoints, worktree isolation, multi-provider switching, local/Ollama — are already table-stakes shipping *for free* inside tools developers already use. That means the bundle, not the parts, must justify the switch.

Calibrated outlook: **moderate** adoption likelihood, with a wide variance. It swings **high** if delivered additively (a CLI/daemon + MCP server and/or VS Code extension that Claude Code, Cursor, and Codex can drive) and **low** if shipped as a from-scratch replacement IDE that gates all value behind a new graph-primary mental model. The headwinds compounding the low case: a documented ~50+ hours of deliberate practice for meaningful (~10%) gains when switching AI editors, plus the poor track record of node/graph UIs with professional developers (medium confidence; drawn from low-code/visual-programming adoption), plus declining developer trust in AI tools even as usage rises (~84% use or plan to use AI; ~29% trust output accuracy).

## 7. Who Adopts & Why (Segments & Wedge)

Adoption will not arrive as a broad wave. It will start with a narrow segment that already feels the pain acutely enough to bolt on third-party tools, then expand only if the integrated workflow earns word-of-mouth.

### 7.1 Demand signal by value prop

Calibrating Spork's dimensions against revealed-preference evidence separates the headline drivers from the table-stakes parity.

| Value prop | Signal | Evidence (confidence) |
|---|---|---|
| Never-lose-agent-work / restore (D3) | **Strong** | Went native everywhere (Claude Code /rewind, Cursor/Replit/Kiro checkpoints) *and* spawned bolt-ons (ccundo ~1.4K stars, Rewind MCP, ckpt) (high) |
| Parallel cross-branch experimentation (D1/D4) | **Strong** | Vibe Kanban ~27K stars, Conductor, Claude Squad, Crystal/Nimbalyst; near-universal worktree support by 2026 (high) |
| Lineage-aware context / handoffs (D6) | **Moderate-strong** | Amp retired compaction for "handoff"; compaction shown to discard ~98% of context (med) |
| Local-first / BYOK / privacy (D4/D5) | **Moderate (table-stakes)** | 44% of orgs cite privacy as top LLM barrier; Ollama ~169K stars — but now baseline, not differentiating (med-high) |
| Multi-provider fast switching (D5) | **Weak (commoditized)** | Native /model, OpenRouter 300+, gateways; not a standalone reason to switch (high) |

The clear read: lead the wedge on D3 + D1/D4 (non-destructive restore and parallel branching), ship D5/local-first quietly as parity, and treat D2's typed auto-running check nodes as the durable, hard-to-copy differentiator — D2 is the genuinely empty quadrant across the entire landscape.

### 7.2 Segments, in priority order

| Segment | Why they adopt | Wedge fit | Beachhead odds |
|---|---|---|---|
| AI-native power users running 2–4 agents | Already self-identified the pain; bolt on ccundo/Conductor/Vibe Kanban today | Direct — they want the unified DAG | High |
| Regulated / privacy-sensitive teams | Local-first + BYOK + Ollama is structurally inaccessible to cloud incumbents (GitLab Duo Self-Hosted precedent) | Strong on D4/D5; needs compliance hardening (SOC2 ~6–12 mo) | Medium |
| jj/Jujutsu & VCS enthusiasts | Appetite for better content-addressed history models (~27K jj GitHub users) | Substrate-aligned | Medium |
| Mainstream VS Code/Cursor users | Need 10x payoff to justify migration + new mental model | Weak as a *replacement*; fine as additive layer | Low (initially) |

The first segment is the beachhead because it has already revealed its willingness to pay (in stars, installs, and setup effort) for the exact gaps Spork closes. Positioning should integrate-with rather than compete-with the agent itself — Claude Code is the satisfaction leader (~46%, medium confidence) — framing Spork as the durable state/graph layer that ccundo, Conductor, and Vibe Kanban only approximate.

### 7.3 The wedge

One demoable promise: *run several agent implementations as parallel branches of one project timeline, then click any node to instantly see its exact working tree and non-destructively restore or branch from it* — with auto-running deterministic sanity/test nodes attached to each edit as durable, diffable artifacts (D1 + D3 + the cheapest slice of D2). Make that the 30-second demo; treat D2-full, D4, and D6 as progressive disclosure so the novel data model is invisible at onboarding. The reliability hook no competitor has: closing the untracked-bash-mutation gap (rm/mv/cp and non-edit-tool changes that every linear checkpoint system silently misses).

## 8. What Would Make It Spread — and What Would Kill It

The decision question is not *whether the problem is real* (it is) but *whether this packaging clears the friction*. Below, each spread-driver and each kill-risk is paired with a concrete mitigation.

### 8.1 Spread-drivers

| # | Driver | Why it spreads | Mitigation to amplify |
|---|---|---|---|
| 1 | A single 10x workflow carried by word-of-mouth | Cursor went $0→~$2B product-led; the navigable DAG-of-restorable-sandboxes is the candidate | Make D1+D3 a shareable, 30-second demo; design DAG views to be shareable artifacts |
| 2 | Non-destructive restore of *everything* | Windsurf's irreversible reverts were a top pain; bolt-on undo ecosystem proves dense demand | Lead with "restore is just an event," including bash-side mutations rivals miss |
| 3 | The integrated whole is genuinely unbuilt | D1+D2+D3 absent across all six prior-art categories; early adopters reward true novelty | State the claim concretely: "Cline checkpoints but project-level and branchable" |
| 4 | Local-first + BYOK + multi-provider tailwind | 44% privacy barrier; regulated-team pull cloud incumbents can't follow | Anchor OSS GTM here; start compliance clock early for the regulated beachhead |
| 5 | Near-zero install friction (if achieved) | Single-binary/zero-dep is the winners' bar (Claude Code native, Ollama, Aider) | Ship a single-binary installer; Open-VSX + one-click VS Code config import |
| 6 | Convergence on the handoff pattern (D6) | Amp retired compaction; market is independently arriving at lineage handoffs | Make handoff docs a first-class, automatic node artifact tied to the work-graph |
| 7 | Open governance + extensibility | Cline's "thriving fork ecosystem" (Apache-2.0) drew contributor inflow | Permissive license; user-definable node types as a community extension surface |

### 8.2 Kill-risks

| # | Risk | Why it kills | Mitigation |
|---|---|---|---|
| 1 | Standalone-IDE distribution penalty | No marketplace network effect; Open-VSX-only; full editor migration ask | Ship the DAG engine first as CLI/daemon + MCP and/or a VS Code extension; graph canvas optional |
| 2 | Conceptual overhead of a new mental model | Developers already grok git branches, checkpoints, worktrees; Neovim/LSP-style onboarding stalls | Zero-config first run; text/list timeline as default, DAG as power-user view; progressive disclosure |
| 3 | Incumbent fast-follow | Graph view copyable in ~1–2 quarters; Anthropic/Cursor own the primitives *and* users; Claude Code issue #32631 already requests native branching | Race the clock with git import/export + MCP so adopters accrue uncopyable per-repo history; moat = D2 + accumulated history, not chrome |
| 4 | Most dimensions are already table-stakes | Checkpoints, worktrees, switching, local all ship free in incumbents | Differentiate on the integrated UX + D2 typed auto-running nodes, not any single primitive |
| 5 | Parallel-agent footguns | "Merge tax," shared DB/port/Docker races, 2GB→~9.8GB worktree storage blowup | Make solving these the feature: resource-aware scheduler, sandboxed (not bare-worktree) state, content-addressed CoW storage |
| 6 | Engineering reliability cliffs | Untracked-mutation closure, large-repo/monorepo snapshot cost (CoW degrades on ext4/NTFS), partial-restore correctness trap | Build on jj's content-addressed op-log; gate promises behind an honest supported-filesystem/resource matrix at onboarding |
| 7 | Heavy OSS maintenance, no installed base | Cross-platform Tauri (3 webviews), per-OS snapshot engine, provider treadmill, tiered sandboxes; OSS dev tools consolidate fast (Roo reportedly shut 2026; Windsurf dismantled in ~72h) | Build on jj substrate; defer D4 sharing, marketplace, microVM isolation to later phases |
| 8 | Local DAG vs team git/PR reconciliation | A graph that fights git/CI stays single-player, capping reach to solo power users | Ship git import/export day one; defer multiplayer sandbox-sharing (D4) until the single-player wedge lands |

### 8.3 Bottom line

The idea would be adopted; the maximalist packaging may not. The calibrated path to spread is **build-but-narrow**: own the engine and the data model, narrow the wedge to one undeniable workflow (D1+D3 plus the cheapest slice of D2), and meet developers where they already are rather than asking them to switch editors and learn a new visual paradigm at once. The single largest swing factor between the moderate and low outcomes is form factor — additive (CLI/MCP/extension) versus replacement IDE.


## 9. Recommendation

**Verdict: BUILD — BUT NARROW.** The integrated whole (D1+D2+D3 as the central experience) is genuinely unbuilt, and the underlying demand is validated by revealed preference, so the concept clears both the novelty and the pull bars. But the novelty is *integration-shaped* and thin at the dimension level — D3/D4/D5/D6 are commoditizing, and Zed DeltaDB already covers ~4–5 of 6 (minus D2) — while the standalone-Tauri-IDE form factor is the single largest adoption risk. So: **build the engine, narrow the packaging and the wedge.**

### 9.1 The five moves

1. **Lead with one undeniable workflow, not the six-dimension vision.** The 30-second demo and all of Phase 1 should be a single thing: *"run N implementations as parallel branches of one project timeline; click any node to see its exact working tree; restore or branch the best one — non-destructively."* That is D1+D3, and it is demoable cold. Treat **D2 typed, auto-running check nodes** as the durable, hard-to-copy differentiator that must ship close behind — because D2 is the only quadrant *nobody* has.

2. **Ship the engine where developers already are — not behind a new editor.** Strongly reconsider delivering the timeline-DAG engine first as a **CLI/daemon + MCP server** (drivable by Claude Code, Cursor, Codex) and/or a **VS Code extension**, with the bespoke graph canvas as an *optional* power-user view and a text/list timeline as the default. This is exactly how jj, Cline, and Claude Code actually got adopted: additive on day one, no migration tax. A from-scratch standalone IDE forfeits the marketplace network effect that carried every recent OSS winner.

3. **Build on jj/Jujutsu's substrate — don't reimplement version control.** jj already gives you an auto-snapshotting working copy, an append-only operation log, and content-addressing. Standing on it removes the heaviest maintenance burden (the native per-OS snapshot engine) and frees the budget for the *typed-node and validation layers that are the actual moat*.

4. **Race the incumbent-copy clock with lock-in that compounds.** The graph view is copyable in ~1–2 quarters and Anthropic/Cursor own both the checkpoint primitives and the users (Claude Code issue #32631 already requests native branching). Counter it by prioritizing **git import/export + MCP** so adopters start accumulating *uncopyable per-repo history* immediately. Defer D4 team-sharing, the marketplace, and microVM isolation (the design already does).

5. **Anchor messaging on the validated pains and the local-first niche — not on commodity features.** Lead with: *non-destructive restore of **everything**, including the out-of-band bash mutations (`rm`/`mv`/`cp`) that every linear-checkpoint competitor silently misses*, and *durable, diffable check nodes*. Add **local-first + BYOK** for the regulated/privacy niche cloud incumbents structurally cannot follow. Do **not** lead with multi-provider switching — it is commodity.

### 9.2 The wedge (narrowest first version)

> A **local-first engine + thin UI** whose single demoable promise is: run several agent implementations as **parallel branches** of one project timeline, then click **any node** to instantly see its exact working tree and **non-destructively restore or branch** from it — with **auto-running deterministic sanity/test nodes** attached to each edit as durable, diffable artifacts (**D1 + D3 + the cheapest slice of D2**). Delivered first as a **CLI/daemon + MCP server** (and/or VS Code extension) drivable by existing agents so it is additive on day one; built on **jj's content-addressed op-log**; DAG canvas optional. Close the **untracked-bash-mutation gap** (the reliability hook no competitor has) and ship **git import/export** so adopters accumulate uncopyable per-repo history from day one.

### 9.3 What NOT to build (the no-build half)

**Do not build the maximalist version first**: a from-scratch standalone IDE that leads with the full six-dimension vision and makes the graph canvas the *mandatory* primary UI. That path maximizes both the engineering surface and the exact dimensions a well-funded frontier incumbent can subsume — while asking developers for a full editor migration (~50+ hours to proficiency) *and* a new graph/typed-node mental model (which has a poor track record with professional developers) at the same time. It is the highest-cost, highest-risk, most-copyable cut of the idea.

---

## Appendix A — Methodology, Sources & Confidence

### A.1 How this assessment was produced

This memo is the output of a structured, adversarial research process rather than a single pass:

- **Prior-art sweep (5 parallel analysts)** across: (1) AI-native IDEs & agent platforms (Cursor, Windsurf, Zed, Copilot Workspace, Devin, Replit, Factory, Augment, Junie, Antigravity); (2) OSS/CLI agents (Aider, Cline, Roo, Continue, OpenHands, Claude Code, Codex CLI, Goose, Kilo, Plandex); (3) emerging/2025–2026 launches via GitHub / Hacker News / Product Hunt search (iloom, Conductor, Vibe Kanban, Crystal, etc.); (4) adjacent DAG/graph paradigms (W&B, MLflow, DVC, Marimo, Observable, n8n, Langflow, Flowise, Dify); (5) version-control & time-travel tools (Jujutsu, Sapling, GitButler, Graphite, Replit history).
- **Adversarial verification (3 independent refuters)** each *tasked with disproving novelty*: a strict-reading refuter, an "assemble-it-from-parts" refuter, and an adjacent-space hunter. All three returned `fullyExists: false` (confidence high / high / medium-high), with GitButler and Zed DeltaDB as the closest named counter-examples.
- **Adoption analysis (3 angles)**: OSS dev-tool adoption drivers & analogs; demand signals specific to Spork's value props; adoption friction, switching cost, and OSS-maintenance risk.

### A.2 Confidence & claims to validate

The *negative* claim ("the integrated whole is unbuilt") is **medium-high confidence** — it survived a deliberate refutation attempt, but the emerging-tools category moves fast and contains many tiny or abandoned projects, so completeness cannot be guaranteed. Before committing significant effort, validate:

| Claim | Stated confidence | How to validate cheaply |
|---|---|---|
| No shipped product delivers D1+D2+D3 as central UX | Medium-high | Re-run the emerging-tools / GitHub / Show HN sweep at build kickoff; watch GitButler and Zed DeltaDB releases. |
| D2 (typed, auto-running check nodes) is the empty quadrant | High | Trial Antigravity, Bernstein, GitButler — confirm none persists typed checks as restorable graph nodes. |
| Demand is "revealed preference," not sentiment | Medium-high | Confirm star/adoption counts for ccundo, Rewind MCP, Vibe Kanban, Conductor at decision time (figures here are point-in-time, mid-2026). |
| Additive packaging (CLI/MCP/extension) lifts adoption to "high" | Medium | Ship the MCP/CLI slice first and measure pull before investing in the standalone canvas. |
| jj substrate meaningfully cuts engineering cost | Medium | Spike: prototype the snapshot/restore layer on `jj` op-log before building bespoke. |

### A.3 Sources

Findings draw on public product documentation, GitHub repositories and issue trackers (e.g., Claude Code issue #32631 requesting native branching), Hacker News / Product Hunt launches, and vendor announcements current to **mid-2026**. Quantitative figures (star counts, install counts, the "44% cite privacy" adoption-barrier statistic, the ~50-hours-to-editor-proficiency estimate) are point-in-time, single-to-few-source signals and are labeled directional, not audited. They are sufficient to support a build/no-build decision but should be refreshed before any public positioning relies on a specific number.

### A.4 Consistency notes

This document was reframed from a commercial market study (v0.1) to a prior-art + adoption memo (v0.2) after the project goal was set to free/open-source. Revenue-oriented sections (TAM/SAM/SOM, pricing, GTM funnel, SWOT-for-revenue) were intentionally removed, not lost — the competitive landscape they contained is preserved and sharpened here as prior-art evidence (§3–§5). The local-first-vs-shared-team tension flagged in v0.1 is now **resolved** in the design (sandbox-as-unit-of-state; see DESIGN.md §19) and is no longer treated as an open market risk, only as a deferred engineering phase.

