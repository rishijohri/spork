# Spork — UI / UX Design Document

> **What this document is.** The authoritative description of how Spork *looks and feels* and how every on-screen element behaves. It defines the visual language, the layout, wireframes for every view, and — crucially — a **complete mapping from every interactable component to (a) the visual change it produces and (b) the action/command it performs**, plus a justification for every non-interactable element. It closes with a **Feature ↔ Plan traceability matrix** so the UI never advertises a capability the [Implementation Plan](IMPLEMENTATION_PLAN.md) has not built.
>
> **What it is not.** Not the system spec (see [DESIGN.md](DESIGN.md), cited throughout as "§n") and not the build order (see [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) / [TODO.md](TODO.md)). For the product vision, see [IDEA.md](IDEA.md).
>
> **Status:** PROPOSAL — to be confirmed before the current UI (`app/`) is rewritten against it.

---

## Table of Contents

1. [Design principles](#1-design-principles)
2. [The "built vs. forward-map" rule (the honesty contract)](#2-the-built-vs-forward-map-rule-the-honesty-contract)
3. [Visual design language](#3-visual-design-language)
4. [Information architecture — the five regions](#4-information-architecture--the-five-regions)
5. [Wireframes](#5-wireframes)
6. [Node-type & status visual encoding](#6-node-type--status-visual-encoding)
7. [Interactive component catalog (component → visual → action)](#7-interactive-component-catalog-component--visual--action)
8. [Non-interactive component catalog (display-only, justified)](#8-non-interactive-component-catalog-display-only-justified)
9. [States: empty, loading, busy, error, disabled](#9-states-empty-loading-busy-error-disabled)
10. [Motion & feedback](#10-motion--feedback)
11. [Accessibility](#11-accessibility)
12. [Responsiveness, resizing & density](#12-responsiveness-resizing--density)
13. [Feature ↔ Plan traceability matrix](#13-feature--plan-traceability-matrix)
14. [Future-phase UI surfaces (designed now, scoped by phase)](#14-future-phase-ui-surfaces-designed-now-scoped-by-phase)
15. [Open decisions to confirm](#15-open-decisions-to-confirm)
16. [Keeping this document live (the phase-completion ritual)](#16-keeping-this-document-live-the-phase-completion-ritual)

---

## 1. Design principles

These six principles resolve every later trade-off. When two layouts compete, the one that better serves these wins.

1. **The graph is the hero.** The DAG canvas is the primary surface (§14.1). Every other region is in service of "what is this node, and what do I do with it." Chrome recedes; the graph gets the light, the space, and the color budget.
2. **Show only what's real (the honesty contract).** No button, tab, or indicator appears unless the [Implementation Plan](IMPLEMENTATION_PLAN.md) has *built* its backing — or it is explicitly a labelled, disabled forward-map affordance. Spork's whole pitch is *trust*; a UI that lies about its capabilities betrays the pitch. See [§2](#2-the-built-vs-forward-map-rule-the-honesty-contract).
3. **Never imply fidelity you don't have.** Uncaptured drift is flagged, irreversible side-effects are warned, a slow checkout is labelled slow (§6.4, §11.4, §14.5). Honesty is a *visual* requirement, not just a backend one.
4. **State is legible at a glance.** A node's family, type, status, staleness, branch, and model read instantly from its card — color, shape, badge, and icon each carry one meaning, never overlapping (§7.3, §14.5).
5. **Schema-driven, not hand-coded.** The legend, node card, details panel, and toolbar gating all derive from each type's `NodeTypeDescriptor` (color, icon, fields, result schema, allowed actions). A new node type is *data, not a frontend release* (§14.5). The four+ built-ins use the exact contract custom types will.
6. **Calm, dense, and fast.** This is a professional tool used for hours. Comfortable-but-dense spacing, a quiet palette, restrained motion, and 60fps at thousands of nodes (§14.3, §A.5). Delight comes from *speed and clarity*, not decoration.

---

## 2. The "built vs. forward-map" rule (the honesty contract)

The user's hard requirement: **the UI must not show a feature that the plan does not implement.** This document enforces it with a single rule used everywhere below:

- **🟢 BUILT** — backed by a working daemon implementation today: one of the 14 frozen IPC commands, the `graph_view` read, the **op-log event stream** (`oplog-event`), or local renderer state. These elements are **implemented in the current (v1) UI build**.
- **🟡 FUTURE-PHASE** — its backing is a *planned-future* phase (P6–P9), a *deferred* item, or a frozen-but-**unproduced** transport (see the ephemeral-channel note below). **This is still a first-class part of this document:** every future-phase surface is **fully designed in [§14](#14-future-phase-ui-surfaces-designed-now-scoped-by-phase)** — wireframes, interactive component → visual → action mappings, and non-interactive justifications — exactly like the v1 surfaces, just marked with the phase that unlocks it. It is simply **not yet *implemented*** in the running UI.

> **This document is not bounded by current implementation progress.** It describes the *whole* product UI/UX across every phase. The 🟢/🟡 tag separates **what is implemented now** from **what is designed now but implemented later** — it never means "undocumented." When a phase (P6, P7, …) completes, its 🟡 surfaces are re-tagged 🟢, folded from §14 into the main body (§4–§13), and *then* built. See [§16 — Keeping this document live](#16-keeping-this-document-live-the-phase-completion-ritual).

Every entry in the catalogs (§7, §8) and the matrix (§13) carries one of these tags. **A 🟡 element is fully specified (in §14) but is not part of the v1 *build*.**

**The 14 BUILT commands** (the entire live action surface), all verified present in `crates/spork-ipc/src/command.rs` with live dispatch handlers in `crates/spork-daemon/src/dispatch.rs`: `NODE_CREATE`, `NODE_RESTORE`, `BRANCH_FORK`, `REF_CREATE`, `REF_MOVE`, `OP_UNDO`, `OP_REDO`, `GC_RUN`, `NODE_DIFF`, `BLOB_READ`, `NODE_RUN_CHECK`, `BRANCH_MERGE`, `GIT_EXPORT`, `GIT_PUSH` — plus the `graph_view` read and the `oplog-event` stream.

> **Note — additive surface.** This is *larger* than DESIGN §A.1's command table, which froze only the Phase-0/1 **minimum** (`node.create/restore`, `branch.fork/merge`, `node.runCheck/diff`, `blob.read`, `op.undo/redo`, `gc.run`). `REF_CREATE`/`REF_MOVE` (distinct from the `REF_CREATED`/`REF_MOVED` *events*), and the F3-UI git actions `GIT_EXPORT`/`GIT_PUSH`, were added behind the frozen IPC seam additively (C2/C3) — so checking against §A.1 alone understates what exists.
>
> **Note — plumbed-but-unproduced transports (the subtle trap).** The `ephemeral` side-channels (`CHAT_TOKENS`, `RUN_STDOUT`) and the `ResultEnvelope` are frozen, serialized, and plumbed end-to-end, **but nothing in any real code path produces them today** — the only caller of `publish_ephemeral` is a test, and the result-envelope hash is computed then discarded (`let _envelope_ref = …`), with no read command exposing it. Therefore **live conversation streaming, live run-output streaming, and detailed typed-result rendering are 🟡, not 🟢** — the wire exists but carries nothing yet. The one result signal that *is* live is the observing node's pass/fail **status badge** on the canvas (derived from `NodeView.status`).

---

## 3. Visual design language

> The current UI is a flat grey-on-grey skeleton (`#141414`/`#1b1b1b`/`#1e1e1e`/`#2a2a2a`), one 13px system font, undifferentiated buttons with `border==background` and **no hover/focus/active styling at all**, unicode glyphs standing in for icons, and raw ULIDs/`SCREAMING_SNAKE` enums shown to the user. The language below replaces all of that.

### 3.1 Theme

**Dark-first**, built entirely on CSS custom properties (design tokens) so a light theme is a later token-swap, not a rewrite. `color-scheme: dark`. (To confirm: dark-only v1 vs. ship light in parallel — see [§15](#15-open-decisions-to-confirm).)

### 3.2 Color — semantic tokens

A layered neutral ramp with a *slight cool undertone* (not flat grey) gives depth via elevation, plus one confident accent and a strict semantic set.

| Token | Value (dark) | Use |
|---|---|---|
| `--bg-base` | `#0d1017` | App background (deepest layer) |
| `--bg-sunken` | `#0a0c12` | Canvas backdrop, wells, inputs |
| `--bg-surface` | `#141822` | Panels (rails, details) |
| `--bg-raised` | `#1b2130` | Cards, popovers, node cards |
| `--bg-overlay` | `#212838` | Modals, menus, tooltips |
| `--border-subtle` | `#232a38` | Hairlines between regions |
| `--border-strong` | `#323b4d` | Card edges, control borders |
| `--fg` | `#e6e9f0` | Primary text |
| `--fg-muted` | `#97a0b3` | Secondary text, metadata |
| `--fg-faint` | `#5e677a` | Disabled, placeholders, ULIDs |
| `--accent` | `#7c8cff` (iris) | Primary action, selection ring, focus |
| `--accent-hover` | `#94a1ff` | Accent hover |
| `--accent-quiet` | `#7c8cff1f` | Accent fills/tints (selection bg) |

**Semantic / status** (one hue per meaning, reused everywhere — node status, activity log, badges):

| Token | Value | Meaning |
|---|---|---|
| `--ok` | `#3fb950` | passed / success |
| `--danger` | `#f85149` | failed / destructive |
| `--warn` | `#d29922` | stale / warning / blocked |
| `--info` | `#58a6ff` | running / informational |
| `--neutral` | `#8b949e` | pending / cancelled |

**Node-type accents** are *not* hardcoded — they come from each type's `NodeTypeDescriptor.color`. The built-in defaults ([§6](#6-node-type--status-visual-encoding)) are chosen to encode *family* by hue: mutating=blue/amber, observing=green family, context=violet.

### 3.3 Typography

| Role | Family | Size / weight |
|---|---|---|
| UI text | `Inter`, then `system-ui` | 13px / 400–500 |
| Section & panel titles | `Inter` | 12px / 600, `letter-spacing: .02em`, uppercase for eyebrow labels |
| Card titles | `Inter` | 13px / 600 |
| Code, diffs, IDs, metrics | `JetBrains Mono`, then `ui-monospace` | 12px / 400 |

A real scale (11 / 12 / 13 / 15 / 18 / 24) replaces today's single 13px. **IDs are never shown raw** — a 26-char ULID renders as a 7-char short form (`…A1B2C3D`) with the full value on hover/copy. Enum tags (`PARENT_CHILD`, etc.) are humanized ("derived from", "validates").

### 3.4 Spacing, radius, elevation

- **Spacing scale (4px base):** `4, 8, 12, 16, 24, 32`. Panels use 16px padding (vs today's 10), controls 8/12.
- **Radius:** controls `6px`, cards `10px`, panels/modals `14px`.
- **Elevation:** three shadow steps (`--elev-1` cards, `--elev-2` popovers, `--elev-3` modals) plus a 1px top-highlight hairline on raised surfaces for a crisp edge. Region separation uses elevation + a single `--border-subtle` hairline, not boxed-in borders.

### 3.5 Iconography

A single open-source line-icon set (**Lucide**) replaces all unicode glyphs. Each `NodeTypeDescriptor` references an icon by name. Icons are 16px in cards/toolbars, 14px inline, `currentColor` so they inherit status/accent color. (Custom node types supply their own icon name through the descriptor.)

### 3.6 Controls (the piece most broken today)

Every control gets the full interaction set the current UI lacks entirely:

- **Variants:** `primary` (accent fill), `secondary` (surface + border), `ghost` (text only), `danger` (red, for Restore/Push). Hierarchy is visible at a glance — a destructive action never looks like a read.
- **States:** `:hover` (lift + lighten), `:active` (press), `:focus-visible` (2px `--accent` ring — an accessibility requirement), `:disabled` (faint + `cursor: not-allowed` + a tooltip giving the reason), `[data-busy]` (inline spinner + dimmed label).
- **Hit targets:** min 28px height (vs today's ~22px). Comfortable padding.

---

## 4. Information architecture — the five regions

Spork keeps the **five-region layout frozen by §14.2** (top bar · left navigator+legend · center canvas · right node-details · bottom **status/run rail**), refined for hierarchy. The fifth region keeps its frozen "status *and* run" dual role: it is refined into a tabbed run pane **plus a thin status sub-strip** — an additive sub-band, **not** a sixth region. The most important refinement fixes a current defect: **the node action toolbar renders only once** (in the top bar, as §14.2 specifies, gated by the selected node) — the duplicate copy currently rendered inside Node-Details is removed.

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ TOP BAR                                                                                     │
│ [spork] project ▸ branch ▾ │  ⎌ undo ↻ redo │  ⌕ search/⌘K  │  ‹node actions…›  │ model ▾ ● │
├──────────────┬───────────────────────────────────────────────────────┬─────────────────────┤
│ LEFT          │ CENTER — GRAPH CANVAS                                  │ RIGHT — NODE-DETAILS │
│ NAVIGATOR     │                                                       │                      │
│               │     ▣──▶✎──▶✓                                          │  ✎  Edit · …A1B2C3D  │
│ Branches      │         └─▶✎──▶⤳                                       │  passed · main · opus│
│  ● main       │              ▲                                        │ ┌──────────────────┐ │
│  ○ feat/x     │     ▣──▶✎────┘                                        │ │Changes│Info│ ·🟡 │ │
│               │                                                       │ ├──────────────────┤ │
│ Node types    │                                  ┌─────────────────┐  │ │  (active tab)    │ │
│  ▣ ✎ ✓ ◎ ⚡ ⤳ │                                  │ ⊕ ⊖ ⤢ minimap   │  │ │                  │ │
│  (filter)     │                                  └─────────────────┘  │ │                  │ │
├──────────────┴───────────────────────────────────────────────────────┴─────────────────────┤
│ RUN RAIL  [ Activity ]  ( Run output 🟡 )                       ⌃ collapse  ⧉ copy  ⌫ clear  │
│ 01:47:13  ✓ Committed …A1B2C3D → spork/…EDIT @ 0f3a91c2                                       │
├──────────────────────────────────────────────────────────────────────────────────────────┤
│ STATUS SUB-STRIP  ● daemon connected  │ branch: main  │ 42 nodes  │ sel: …A1B2C3D            │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

**Region roles**

| Region | Role | Key refinements over today |
|---|---|---|
| **Top bar** | Global context + global actions + node-action toolbar (single instance) + default-model selector + daemon status. | Split into zones with separators; project/branch identity added; undo/redo + search/⌘K added; the action toolbar is selection-gated and lives here only. |
| **Left navigator** | Real navigation: **Branches** (from `graph_view.refs`) and a **node-type legend that doubles as a canvas filter**. Collapsible. | Today it is a static legend only; it gains branch list + click-to-filter and becomes collapsible to give the canvas room. |
| **Center canvas** | The DAG. Rich node cards, humanized edges, zoom/fit/minimap controls, search, and a real empty state. | Today: bare React-Flow default nodes (icon+label), raw enum edge labels, no controls, no empty state. |
| **Right node-details** | The selected node. **v1 (🟢) tabs: Changes · Info.** Forward-map tabs (🟡): **Conversation** (P6) and **Results-detail** (P7) — see §5.5/§5.7. Lean header (no duplicate toolbar). | Real before/after diff (today it diffs against empty) and a real Info tab replace the placeholder dump; conversation/typed-results surfaces are documented but await their producers. |
| **Bottom status/run rail** (region 5) | A collapsible pane with the **Activity** tab (🟢 — op-log events + action outcomes) and a forward-map **Run output** tab (🟡 — needs a streaming `RUN_STDOUT` producer); copy/clear per tab. Plus the **status sub-strip** below it (daemon connection, current branch, node count, selection). | Today both notional streams are jammed into one 160px scroll area; v1 ships only the real one (Activity) and adds the persistent status sub-strip. |

---

## 5. Wireframes

ASCII wireframes for every view. Boxes are regions/cards; `▾` = menu/dropdown, `▸` = expandable, `●/○` = selected/unselected, `[ ]` = button, `⌕` = search.

### 5.1 Top bar (zoomed)

```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ ◧ spork   myproject ▸ [ main ▾ ]   │  [⎌ Undo] [↻ Redo]   │  [ ⌕ Search nodes…  ⌘K ]          │
│                                     │                       │                                   │
│   ‹ when a node is selected: ›   [↩ Restore][▶ Run check ▾][⑂ Branch][⤳ Merge…][⎘ Commit][⇪ Push]│
│                                                                       [ Claude Opus 4.8 ▾ ]  ● │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
   zone A: identity + branch     zone B: history    zone C: search    zone D: node actions   zone E: model + status
```

- **Zone D (node actions)** is empty/greyed with no selection; populates and gates per the selected node's family ([§7.1](#71-top-bar)). Single instance — not repeated in Node-Details.
- **Model selector (zone E)** sets the *default model for new nodes only* (§12.3); a tooltip says so. The `●` is the daemon connection dot.

### 5.2 Left navigator

```
┌────────────────────┐
│ BRANCHES        + ⌃ │   ← "+" = new ref/branch (REF_CREATE); ⌃ = collapse panel
│  ● main             │   ← current branch (bold, accent bar)
│  ○ feat/login       │
│  ○ exp/refactor     │
│                     │
│ NODE TYPES      ⌕   │   ← legend + filter; ⌕ filters the canvas
│  ▣ Snapshot     12  │   ← swatch · icon · label · live count; click = toggle filter
│  ✎ Edit          8  │
│  ✓ Validation    5  │
│  ◎ Sanity        9  │
│  ⚡ Stress        2  │
│  ⤳ Merge         1  │
│  · Context       3  │   ← Plan/Conversation (context family)
└────────────────────┘
```

- Branch rows come from `graph_view.refs` (🟢). Clicking a branch **highlights/focuses** its lineage on the canvas (local). "+" opens the New-ref popover (`REF_CREATE`, 🟢).
- Node-type rows come from the `NodeTypeRegistry` descriptors (🟢, schema-driven). The count is computed from the loaded graph. Clicking a row toggles a **canvas filter** (dims non-matching nodes — local). This fixes the "legend has no canvas linkage" defect.

### 5.3 Node card anatomy (the centerpiece)

```
         selection ring (accent)                       status badge (effectiveStatus)
        ╭──────────────────────────────╮              ┌──────────────────────┐
   ┌────┃ ✎  Edit                  ⬤ ✓ ┃────┐         │ ⬤✓ passed            │
   │ ▎  ┃ "add login validation"       ┃    │         │ ⬤◷ running (pulse)   │
   │ ▎  ┃ ⎇ feat/login  ·  ◆ Opus 4.8  ┃    │         │ ◌ pending            │
   │ ▎  ┃ +3 ~2  files                 ┃    │         │ ⬤✕ failed            │
   └────┸──────────────────────────────┚────┘         │ ⬤✓⚠ passed · stale  │
     ▎ = type color stripe (descriptor.color)          │ ⊘ blocked / cancelled│
                                                        └──────────────────────┘
```

| Card element | Source | Tag |
|---|---|---|
| Type color stripe + icon | `descriptor.color` / `descriptor.icon` for `node.kind` | 🟢 |
| Kind label ("Edit") | `descriptor.label` | 🟢 |
| Status badge | derived **effectiveStatus** = (`status` × `isStale`) (§7.3) | 🟢 |
| Branch chip (`⎇ feat/login`) | `node.branchId` | 🟢 |
| Model chip (`◆ Opus 4.8`) | `node.model` (humanized) | 🟢 |
| Snapshot dot (owns restorable state) | `node.ownsSnapshot` | 🟢 |
| Title/summary (`"add login validation"`) | **additive** view-model field | 🟡 (additive read; [§14](#14-future-phase-ui-surfaces-designed-now-scoped-by-phase)) |
| Diff stat (`+3 ~2`) | **additive** view-model field, or lazy `NODE_DIFF` on hover | 🟡 (additive read) |
| Stale reason on hover | **additive** view-model field | 🟡 (additive read) |

> The v1 card ships with everything marked 🟢 — already a large jump from today's "glyph + label". Title/diff-stat/stale-reason need read-only denormalized fields added to the `graph_view` projection (no event-log or schema change — additive per C3); they are flagged so the card never *claims* data it doesn't have.

### 5.4 Canvas with chrome + empty state

```
ACTIVE GRAPH                                          EMPTY PROJECT
┌───────────────────────────────────────────┐        ┌───────────────────────────────────────────┐
│                          ┌──────────────┐  │        │                                             │
│  ▣ ─▶ ✎ ─▶ ✓             │ minimap      │  │        │              ◧                              │
│       │     (validates)  │  ░░▓░        │  │        │      No work yet                            │
│       └─▶ ✎ ─▶ ⤳         │  ░░░░        │  │        │  Create your first node to start the graph. │
│            ▲   (merge)   └──────────────┘  │        │                                             │
│  ▣ ─▶ ✎ ───┘                               │        │            [ Open project ]                 │
│                                            │        │                                             │
│                        [⊕][⊖][⤢ fit][⌕]    │        │                                             │
└───────────────────────────────────────────┘        └───────────────────────────────────────────┘
  zoom in/out · fit-to-view · search           CTA-driven empty state (open_project), not a blank grid
```

- Edges are color-coded by `edgeType`, **humanized** on hover ("validates", "derived from") — never raw `VALIDATES` — and encode **lineage vs. attachment** (see §6.3): **solid** for branch lineage (`PARENT_CHILD` neutral, `BRANCH` accent, `MERGE_PARENT` amber) and **dotted** for attachment/observation (`VALIDATES`/`CHECKS`/`STRESSES` green, `DERIVED_FROM` violet). So it's instantly clear which branch a node *belongs to* vs. which node it merely *refers to*.
- Consecutive auto-`Snapshot` (drift) nodes **collapse** into a single expandable cluster (§7.4) to cut clutter.
- Controls: zoom ⊕/⊖ (🟢 local), fit/recenter ⤢ (🟢 local), minimap toggle (🟢 local), canvas search (🟢 local, filters loaded nodes).
- **Empty state is honest about creation.** A project's nodes are *produced by the daemon* — drift capture as you edit, agent runs — not minted by the renderer: `create_node` requires a real content `snapshot_hash` (verified in `spork-graph`), which the UI cannot fabricate, and there is **no capture IPC command** in the 14. So the empty state offers **Open project** (the backed `open_project` path, 🟢) + guidance, **not** a fabricated "New node". Creating a snapshot/import node from the UI is **🟡 forward-map** (needs a capture/snapshot command); agent-driven Edit creation is 🟡 (P6, provider routing).

### 5.5 Node-Details — Conversation tab — 🟡 FORWARD-MAP (P6), NOT in v1

> **Why this is not in v1.** The `CHAT_TOKENS` ephemeral channel is frozen and plumbed end-to-end, but **nothing produces a token today** — the daemon invokes no provider (provider routing is P6), and the only caller of `publish_ephemeral` in the repo is a test. There is also no transcript-read command. So a "live conversation" would be a permanently-empty surface: showing it would break the honesty contract. The tab below is the *target design* for when P6 lands, drawn here so the panel layout is forward-compatible. **It does not render in v1.**

```
┌─────────────────────────────────────────┐  🟡 ENTIRE TAB = FORWARD-MAP (P6)
│ ✎ Edit  ·  …A1B2C3D  ⧉    passed · stale ⚠ │  ← lean header: icon, short id (copy), effectiveStatus (🟢)
│ ┌─────────────────────────────────────┐ │
│ │ Changes │ Info │ ·Conversation(P6) │  │ │  ← v1 tab bar is Changes·Info; Conversation arrives in P6
│ └─────────────────────────────────────┘ │
│  ┌─ assistant ───────────────────────┐  │
│  │ I'll add a guard in auth.ts…       │  │  ← role-labelled messages, fed by CHAT_TOKENS once produced
│  └────────────────────────────────────┘  │
│  ▌ streaming…                            │  ← live cursor (only once a provider emits tokens, P6)
│  ┌──────────────────────────────────┐    │
│  │ Message the agent…        (P6)    │    │  ← composer needs the agent-turn loop (P6/P7)
│  └──────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

- **Live conversation stream (🟡, P6):** depends on a provider emitting `CHAT_TOKENS`. The transport (bus → Tauri forward → renderer ingest) is built; the *producer* is not.
- **Persistent/historical transcript (🟡, P7):** fetching a *past* node's full conversation needs a transcript-read command (history MCP `get_node_transcript`) — not in the 14 commands.
- **Composer (🟡, P6/P7):** a "send a prompt → agent turn" loop needs provider routing + context compilation.
- **v1 reality:** there is no agent conversation yet (a `NODE_CREATE` carries a pre-computed payload, with no model invocation). The Conversation tab appears when P6 ships.

### 5.6 Node-Details — Changes (diff) tab

```
┌─────────────────────────────────────────┐
│ [Changes] │ Info │              (v1 tabs) │  ← v1 Node-Details tabs are Changes · Info
│ Comparing against:  [ parent ▾ ]          │  ← baseline picker (NODE_DIFF `against`)
│ ┌─ changed paths (3) ──────────────────┐  │
│ │ +  src/auth/login.ts                  │  │  ← +/~/− derived by presence in parent vs node tree
│ │ ~  src/auth/index.ts                  │  │
│ │ −  src/auth/legacy.ts                 │  │
│ └────────────────────────────────────── │
│ ┌─ src/auth/index.ts ──────────────────┐ │
│ │  12 │ - const x = old()              │ │  ← Monaco diff: original = parent tree blob,
│ │  12 │ + const x = guard(old())       │ │              modified = node tree blob
│ │ …                                    │ │
│ └──────────────────────────────────────┘ │
└─────────────────────────────────────────┘
```

- Changed-path set: `NODE_DIFF { nodeId, against }` (🟢). Default baseline = first parent; the picker lets you diff against any node (`against`, 🟢).
- **Real before/after** (🟢, fixes the current empty-original bug): `original` = `BLOB_READ(parentSnapshotTree, path)`, `modified` = `BLOB_READ(nodeSnapshotTree, path)`. Add/modify/delete is derived from each side's presence.
- Monaco gets a **usable viewport** (fills the panel, not 240px), with language inferred from file extension (today it's hardcoded `plaintext`). Binary blobs show "binary · N bytes", not a garbage diff.

### 5.7 Node-Details — Results-detail tab — 🟡 FORWARD-MAP (P7), NOT in v1

> **Why the detail is not in v1.** Running a check **is** built (`NODE_RUN_CHECK`, P5) and its outcome **is** visible in v1 — as the observing node's pass/fail **status badge** on the canvas and its typed edge (`VALIDATES`/`CHECKS`/`STRESSES`). But the rich `ResultEnvelope` *contents* (per-unit pass/fail, coverage, latency percentiles, throughput, `{ruleId,file,line,fixable}` violations) **never reach the renderer**: the envelope is stored in the CAS and its hash is then discarded (`let _envelope_ref = …`), the payload is not in the view-model, and there is **no result-read command**. So the detail panel below is the target for when a result-read surface lands (P7); in v1 the result lives entirely in the canvas status badge.

```
┌─────────────────────────────────────────┐  🟡 RESULT *DETAIL* = FORWARD-MAP (P7)
│ ┌─ Validation · run …RUNID ────────────┐ │   ← typed ResultEnvelope, per descriptor result schema
│ │  ✓ 48 passed   ✕ 2 failed   ◌ 1 skip  │ │
│ │  coverage 81%                         │ │
│ │  ▸ test_login_rejects_empty   ✕       │ │   ← expandable failing units
│ └──────────────────────────────────────┘ │
│ ┌─ Stress (if observing=stress) ───────┐ │   ← perf metrics when the result type carries them
│ │  p50 12ms · p95 40ms · p99 110ms      │ │
│ │  throughput 8.2k/s · peak 220MB       │ │
│ └──────────────────────────────────────┘ │
│ run history:  ● now   ○ −1   ○ −2          │   ← multi-run history browse (also P7)
└─────────────────────────────────────────┘
```

- **v1 (🟢):** the check's pass/fail is the observing node's **effectiveStatus badge** on its canvas card (and in the Info tab's Status row). That is the only result signal exposed today.
- **Result-detail rendering (🟡, P7):** needs a result-read command/view-model field that surfaces the stored `ResultEnvelope` (F4 envelope + P5 runners produce it; nothing reads it back yet).
- **Run-history browsing (🟡, P7):** keep-last-N history needs the same read surface.

### 5.8 Node-Details — Info tab

```
┌─────────────────────────────────────────┐
│ Changes │ [Info] │              (v1 tabs) │
│  Type        Edit  (mutating)             │  🟢
│  Status      ✓ passed · ⚠ stale           │  🟢 effectiveStatus
│  Branch      ⎇ feat/login                  │  🟢
│  Model       ◆ Claude Opus 4.8            │  🟢
│  Snapshot    ◆ owns · tree 0f3a…91c2  ⧉   │  🟢 ownsSnapshot + snapshotHash (short, copy)
│  Parents     ▸ …9F8E7D6   ▸ …5C4B3A2      │  🟢 parentIds → click to select
│  ─────────────────────────────────────    │
│  Cost        $0.0142  (1.2k in / 3.1k out)│  🟡 cost ledger = P6
│  Attribution agent · confidence 0.97 ✎    │  🟡 editable AttributionRecord UI = P7
│  Handoff     [ View handoff doc ]          │  🟡 handoff view = P7
│  Effects     ⚠ 1 external write recorded   │  🟡 effects-log surface = P7
└─────────────────────────────────────────┘
```

- The 🟢 block (type/status/branch/model/snapshot/parents) uses only present `NodeView` fields. Parent rows are clickable (select that node — local).
- The 🟡 block (cost, editable attribution, handoff, effects) is documented here for placement but **does not render in v1**.

### 5.9 Bottom status/run rail (region 5)

The v1 rail is the **Activity** tab (🟢). The **Run output** tab is drawn but 🟡 (forward-map) — it needs a runner that *streams* onto the `RUN_STDOUT` channel; today checks run synchronously and the runner consumes stdout as one buffer for normalization, so no frame is ever emitted mid-run.

```
v1 — Activity (🟢)                                              forward-map — Run output (🟡)
┌──────────────────────────────────────────────────────────┐  ┌────────────────────────────────────┐
│ [ Activity ]   ( Run output 🟡 )      ⌃ collapse  ⧉  ⌫    │  │  Activity   [ Run output 🟡 ]       │
│ ─────────────────────────────────────────────────────────│  │ ───────────────────────────────────│
│ 01:47:13 ✓ Committed …A1B2C3D → spork/…EDIT @ 0f3a91c2     │  │ $ cargo test           (needs a     │
│ 01:47:14 ✓ Pushed spork/…EDIT → origin                    │  │ running 50 tests ...    streaming   │
│ 01:47:15 ⚠ Push failed: capability net.connect denied     │  │ ▌                       RUN_STDOUT  │
└──────────────────────────────────────────────────────────┘  └────────────────────────────────────┘
│ STATUS SUB-STRIP  ● connected · branch main · 42 nodes · sel …A1B2C3D                            │
```

- **Activity (🟢)** = op-log events (`oplog-event`) + toolbar-action outcomes. This is the real, producing stream — the one we already see working ("Committed/Pushed/…"). Each line has a **severity icon** (not color-only — accessibility), a timestamp, and links the short id back to its node (click = select).
- **Run output (🟡, needs streaming producer):** the channel and renderer ingest exist; the runner does not yet stream. Drawn for placement only.
- The **status sub-strip** sits beneath the rail and stays visible even when the rail is collapsed (⌃). Copy (⧉) / clear (⌫) act on the active tab.

### 5.10 Overlays & modals

All modals share the elevation-3 surface, a title, body, and a right-aligned action row (secondary + primary). Destructive primaries use the `danger` variant.

**(a) New branch** — `BRANCH_FORK` (🟢)
```
┌ New branch ─────────────────────────────┐
│ From node:  ✎ …A1B2C3D  "add login…"     │
│ Name:       [ feat/login-v2___________ ] │  ← editable (today it auto-names branch/<id>)
│                          [ Cancel ] [ Create branch ] │
└─────────────────────────────────────────┘
```

**(b) Merge + conflict resolution** — `BRANCH_MERGE` (🟢)
```
┌ Merge ──────────────────────────────────┐        ┌ Resolve conflicts (2) ──────────────────┐
│ From: ⎇ feat/login  →  Into: [ main ▾ ]  │   →    │ src/auth/index.ts                        │
│ 3-way from nearest common ancestor.      │        │ ┌ base ─┐ ┌ ours ─┐ ┌ theirs ─┐         │
│                  [ Cancel ] [ Merge ]    │        │ │ …     │ │ …     │ │ …       │  ← Monaco │
└─────────────────────────────────────────┘        │ └───────┘ └───────┘ └─────────┘   3-way   │
   on conflict, result carries a conflict set →     │              [ Cancel ] [ Apply & merge ] │
   no half-node is created until resolved (§A.4)     └─────────────────────────────────────────┘
```
- Clean merge → a materializable `Merge` node appears (`MERGE_PERFORMED`). Conflicts → the conflict set rides the result; the 3-way editor (base/ours/theirs via `BLOB_READ` from the three trees) collects a `resolution` that is resubmitted. No half-node until every conflict is resolved.

**(c) Restore confirmation** — `NODE_RESTORE` (🟢), `danger`
```
┌ Restore this node? ─────────────────────┐
│ Brings the working tree + conversation   │
│ back to ✎ …A1B2C3D. This is an EVENT,    │  ← restore is non-destructive (§6.4)
│ not an overwrite — your current line      │
│ survives as a branch.                     │
│ ⚠ External side-effects (DB writes, pushed │  ← honest irreversible-effects warning (§11.4)
│   commits, paid API spend) are NOT undone. │
│                  [ Cancel ] [ Restore ]   │
└─────────────────────────────────────────┘
```

**(d) Commit to Git / Push** — `GIT_EXPORT` / `GIT_PUSH` (🟢)
```
┌ Commit to Git ──────────────────────────┐     ┌ Push ───────────────────────────────────┐
│ Project ✎ …A1B2C3D to a real Git commit. │     │ Remote: [ origin____ ]                   │
│ Branch: [ spork/…EDIT________ ]          │     │ ⚠ Needs the `net.connect` capability and │  ← net.connect is
│ (.git HEAD/index stay untouched.)        │     │   uses your existing git credentials.    │     deny-by-default
│                 [ Cancel ] [ Commit ]    │     │                 [ Cancel ] [ Push ]      │
└─────────────────────────────────────────┘     └─────────────────────────────────────────┘
```

**(e) Capability denied** (🟢 surface) / **inline grant** (🟡)
```
┌ Action needs a capability ──────────────┐
│ "Push" requires:  net.connect            │  🟢 honest denial surface (deny-by-default broker)
│ Status: DENIED                           │
│ Grant it in your daemon capability config │  🟡 one-click inline grant needs a grant command
│ to enable network actions.                │     (not in the 14 frozen commands) — see §14
│                          [ Got it ]      │
└─────────────────────────────────────────┘
```

**(f) Command palette** — ⌘K (🟢 local; runs only BUILT commands)
```
┌ ⌘K ─────────────────────────────────────┐
│ ⌕ commit________________________________ │
│  ⎘ Commit selected node to Git           │  ← fuzzy list of BUILT actions + node-jump
│  ⇪ Push selected node                    │
│  ⑂ New branch from selected node         │
│  ↩ Restore selected node                 │
└─────────────────────────────────────────┘
```

**(g) Run GC (maintenance)** — `GC_RUN` (🟢)
```
┌ Garbage collection ─────────────────────┐
│ [✓] Dry run (preview only)               │  ← dry-run first; live run emits GC_PERFORMED
│ Reclaimable: 128 objects · 42.0 MB       │  ← from CommandResult::Gc {reclaimable, bytes}
│                 [ Close ] [ Run GC ]     │
└─────────────────────────────────────────┘
```

### 5.11 Context menus (canvas node & branch row)

Right-click (or the `⋯` affordance) opens a context menu. Items are tagged; 🟡 items render only once their phase lands.

```
NODE context menu (right-click a card)            BRANCH-ROW context menu (right-click a ref)
┌──────────────────────────────┐                  ┌──────────────────────────────┐
│ ↩ Restore             🟢      │                  │ ◎ Set as HEAD          🟢     │  ← REF_MOVE
│ ▶ Run check          ▸ 🟢     │                  │ ⑂ New branch here      🟢     │  ← REF_CREATE / BRANCH_FORK
│ ⑂ New branch          🟢      │                  │ ⤳ Merge into…          🟢     │  ← BRANCH_MERGE
│ ⤳ Merge…              🟢      │                  │ ⌕ Focus lineage        🟢     │  ← local
│ ⊟ Diff against…       🟢      │                  └──────────────────────────────┘
│ ⎘ Commit to Git       🟢      │
│ ⇪ Push                🟢      │                  v1 shows only 🟢 rows; 🟡 rows are
│ ──────────────────────────── │                  hidden until their phase (see §14).
│ ⧉ Copy node id        🟢      │
│ ⤓ Check out node     🟡 (P7)  │  ← materialize a worktree (F4 seam exists; no IPC cmd)
│ 📌 Pin node           🟡 (P7)  │  ← protect from GC/eviction
│ ✦ New edit with agent 🟡 (P6) │  ← agent-turn loop
└──────────────────────────────┘
```

The node menu mirrors the top-bar node actions (single source of truth for gating via `enabledWhen`); the branch menu mirrors the navigator's ref actions. Every 🟢 item here also appears in the §7 catalog.

### 5.12 Disconnected / reconnecting daemon

The daemon is a separate process (§14.1), so "daemon unreachable" is a first-class user state. The graph stays visible (last view-model), actions disable, and a non-blocking banner + status dot communicate state. No fake success while disconnected.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ TOP BAR  …                                          actions disabled (greyed)  │
├──────────────────────────────────────────────────────────────────────────────┤
│  ⚠ Lost connection to the Spork daemon — reconnecting…           [ Retry now ] │  ← amber banner
│                                                                                │
│        (last-known graph remains visible but read-only; panning still works)   │
├──────────────────────────────────────────────────────────────────────────────┤
│ STATUS SUB-STRIP   ◌ reconnecting…   │ branch: main   │ last update 12s ago     │  ← dot: reconnecting
└──────────────────────────────────────────────────────────────────────────────┘
```

Status-dot states (only these three, user-facing): **● connected** (`--ok`), **◌ reconnecting** (`--warn`, pulsing), **● disconnected** (`--danger`). *(A "mock" badge appears only when the app runs in a browser dev harness without a Tauri runtime — a developer affordance, never shown in the packaged desktop app.)*

### 5.13 Settings (popover)

A lightweight settings popover (gear in the top bar) — home for the few real v1 preferences. All are **local renderer state (🟢)**; engine-health/marketplace settings are 🟡 (P8).

```
┌ Settings ───────────────────────────────┐
│ Appearance                               │
│   Theme       ( Dark ● )  Light 🟡(later) │  ← dark-only v1; light is a token-swap later
│   Density     ( Comfortable ● ) Compact   │  ← 🟢 local; affects spacing scale
│ Layout                                   │
│   [ Reset panel sizes ]                  │  ← 🟢 local; clears persisted panel widths
│ Models                                   │
│   Default model for new nodes  [ Opus ▾ ]│  ← same binding as the top-bar selector
│ ──────────────────────────────────────── │
│   Engine health · Extensions      🟡 (P8) │  ← forward-map settings sections
└──────────────────────────────────────────┘
```

---

## 6. Node-type & status visual encoding

### 6.1 Built-in descriptor palette (data-driven defaults)

Color encodes **family**; icon + label encode **type**. These are the *default* `NodeTypeDescriptor` values — custom types bring their own and render identically (§14.5).

| Type | Family | Color (default) | Icon (Lucide) | Owns snapshot | Notes |
|---|---|---|---|---|---|
| Snapshot | mutating | `#94a3b8` slate | `camera` | ✓ | `origin` badge: auto-drift / manual / import (§7.4); consecutive auto-drift collapse |
| Edit | mutating | `#60a5fa` blue | `pencil` | ✓ | branch point; shows diff stat |
| Merge | mutating | `#f59e0b` amber | `git-merge` | ✓ | ≥2 parents; clean-only |
| Validation | observing | `#34d399` green | `check-check` | ✗ | `VALIDATES` edge; pass/fail/coverage |
| Stress | observing | `#2dd4bf` teal | `gauge` | ✗ | `STRESSES` edge; latency/throughput |
| Sanity | observing | `#a3e635` lime | `scan-line` | ✗ | `CHECKS` edge; auto-runs after edits |
| Plan / Conversation | context | `#a78bfa` violet | `list` / `messages-square` | ✗ | no snapshot (not clickable-to-state) |

### 6.2 effectiveStatus tokens (cross-type, §7.3)

Exactly one token per `(status, isStale)` pair, computed in the renderer from `NodeView.status` + `NodeView.isStale` (🟢):

| Token | Visual | Meaning |
|---|---|---|
| passed | `--ok` filled ● + check | green |
| passed · stale | `--ok` fill + `--warn` ring + "stale" chip | passed but inputs changed |
| failed | `--danger` filled ● + ✕ | red |
| failed · stale | `--danger` fill + `--warn` ring | failed and stale |
| running | `--info` ● with pulse animation | in progress |
| pending | hollow `--neutral` ◌ | queued/not started |
| blocked | `--warn` ⊘ (lock) | gated/can't proceed |
| cancelled | `--fg-faint` ● struck-through | cancelled |

### 6.3 Edge encoding — lineage (solid) vs. attachment (dotted)

The single most important "where am I" cue on the canvas: an edge's *style* says whether a node is **part of a branch's lineage** or merely **refers to** another node. This makes automatic branching (DESIGN §6.6) legible — you can see at a glance which branch a node belongs to.

| Edge type | Meaning | Style | Color |
|---|---|---|---|
| `PARENT_CHILD` | lineage — a step in the branch | **solid** | neutral |
| `BRANCH` | lineage — fork origin of a branch | **solid** | accent |
| `MERGE_PARENT` | lineage — a merge's parent | **solid** | amber (`--warn`) |
| `VALIDATES` / `CHECKS` / `STRESSES` | observation — a check *about* a node | **dotted** | green (`--ok`) |
| `DERIVED_FROM` | attachment — read-only context (analysis/plan) *derived from* a node | **dotted** | violet |

Read-only agent work (analysis, planning — DESIGN §6.6) attaches via a **dotted** `DERIVED_FROM` edge and stays on the current line; only snapshot-owning work participates in fork-on-divergence and draws **solid** lineage edges. The dotted/solid rendering is 🟢 (it styles edge types that already exist); the analysis/plan *nodes* that use `DERIVED_FROM` arrive with the agent-run path (🟡 P6).

---

## 7. Interactive component catalog (component → visual → action)

Every clickable/keyboard-operable element, its trigger, the **visual change** it produces, the **action/command** it performs, and its tag. (🟢 = live v1; 🟡 = forward-map, not in v1.)

### 7.1 Top bar

| Component | Trigger | Visual change | Action / command | Tag |
|---|---|---|---|---|
| Project / branch switcher `▾` | click | Opens branch menu; selecting marks it current (accent), refocuses canvas lineage | Read `graph_view.refs`; select = local focus | 🟢 |
| Undo | click / ⌘Z | Button flashes; graph reverts last op; activity line | `OP_UNDO {opId:null}` → `OP_UNDONE` | 🟢 |
| Redo | click / ⇧⌘Z | Graph re-applies; activity line | `OP_REDO {opId:null}` → `OP_REDONE` | 🟢 |
| Search / ⌘K | click / ⌘K | Opens command palette overlay (5.10f) | Local; dispatches only BUILT commands | 🟢 |
| **Restore** | click (node selected, mutating + ownsSnapshot) | Opens Restore confirm (5.10c); on confirm, working tree + conversation move; `RESTORE_PERFORMED`; activity line | `NODE_RESTORE {nodeId}` | 🟢 |
| **Run check ▾** | click → Validate / Stress / Sanity | A new observing child appears with a typed edge and a pass/fail **status badge** (`RESULT_RECORDED`); detailed result body is 🟡 (§5.7) | `NODE_RUN_CHECK {targetNodeId, spec:{kind}}` | 🟢 |
| **Branch** (manual) | click (any selection) | Opens New-branch modal (5.10a); on create, a new ref appears in navigator | `BRANCH_FORK {fromNodeId, name}` → `BRANCH_FORKED` | 🟢 |
| *Auto-branch on work* (fork-on-divergence) | starting agent/edit work from a non-tip node | a new branch is forked automatically; **Branch** above is the manual override | daemon policy (DESIGN §6.6) | 🟡 P6/P7 |
| **Merge…** | click (a branch context) | Opens Merge modal (5.10b); clean → Merge node; conflict → resolver | `BRANCH_MERGE {intoRef, fromNodeId, resolution?}` | 🟢 |
| **Commit to Git** | click (ownsSnapshot) | Opens Commit modal (5.10d); on commit, success activity line w/ branch+SHA | `GIT_EXPORT {nodeId, branch?}` → `Git` result | 🟢 |
| **Push** (danger) | click (ownsSnapshot) | Opens Push modal (5.10d); success or **explicit denial** (5.10e) on `net.connect` deny | `GIT_PUSH {nodeId, remote?}` → `Git` result | 🟢 |
| New ref/branch (palette/nav `+`) | click | New-ref popover; ref appears | `REF_CREATE {name, kind, to}` → `REF_CREATED` | 🟢 |
| Model selector `▾` | click | Provider-grouped menu; selecting sets default-for-new-nodes; tooltip clarifies scope | Local `setDefaultModel` (stamped on next `NODE_CREATE`) | 🟢 |
| Settings `⚙` | click | Opens settings popover (5.13) | Local | 🟢 |
| Daemon status dot `●` | hover | Tooltip: connected / reconnecting / disconnected (5.12) | Local (connection state) | 🟢 |
| Cost / spend indicator | — | — | per-node + per-branch cost ledger | 🟡 P6 |

### 7.2 Left navigator

| Component | Trigger | Visual change | Action / command | Tag |
|---|---|---|---|---|
| Branch row | click | Row marked current; canvas focuses that ref's lineage | Local focus (reads `refs`) | 🟢 |
| Branch row → "set HEAD" | context action | HEAD ref moves | `REF_MOVE {name:"HEAD", to}` | 🟢 |
| Node-type legend row | click | Toggles filter; non-matching nodes dim on canvas; row shows active state | Local canvas filter | 🟢 |
| Type-filter search `⌕` | type | Filters legend + canvas | Local | 🟢 |
| Collapse panel `⌃` | click | Panel collapses to a thin rail; canvas widens | Local layout | 🟢 |

### 7.3 Canvas

| Component | Trigger | Visual change | Action / command | Tag |
|---|---|---|---|---|
| Node card | click | Selection ring + elevation; Node-Details loads; status strip shows id | Local select → triggers `NODE_DIFF` for Changes tab | 🟢 |
| Node card | double-click | Centers + zooms to node | Local | 🟢 |
| Collapsed Snapshot cluster | click | Expands to individual drift Snapshot nodes | Local | 🟢 |
| Edge | hover | Humanized label tooltip ("validates"); edge highlights | Local | 🟢 |
| Zoom ⊕ / ⊖ | click / scroll | Canvas scales | Local | 🟢 |
| Fit / recenter ⤢ | click | `fitView` to all nodes | Local | 🟢 |
| Minimap | click region | Pans viewport | Local | 🟢 |
| Canvas search ⌕ | type | Matching nodes highlight; others dim | Local | 🟢 |
| Empty-state "Open project" | click | Opens a project; graph_view refetches; nodes appear | `open_project` + `graph_view` refetch | 🟢 |
| "New node / Import state" (capture) | — | (creating a root snapshot needs a real content hash the UI can't mint) | a capture/snapshot IPC command | 🟡 (no capture cmd) |
| "Check out this node" | context menu (5.11) | Materializes a worktree (labelled slow) | worktree-checkout command | 🟡 (F4 seam built; no IPC cmd) |
| "Pin node" | context menu (5.11) | Pin badge; protects from GC | pin command | 🟡 (no IPC cmd) |
| "New edit with agent" | context menu (5.11) | Agent turn runs, streaming into the new node | agent-turn loop | 🟡 P6 |

### 7.4 Node-Details

| Component | Trigger | Visual change | Action / command | Tag |
|---|---|---|---|---|
| Tab: **Changes** / **Info** | click | Active tab gets accent underline + bg; body swaps | Local tab state | 🟢 |
| Tab: Conversation / Results-detail | — | (drawn for placement; tabs appear when their phase lands) | — | 🟡 P6 / P7 |
| Short id `⧉` | click | Copies full ULID; "copied" toast | Local clipboard | 🟢 |
| Parent row (Info) | click | Selects that parent node | Local select | 🟢 |
| Diff baseline picker `▾` | change | Re-runs diff against chosen node | `NODE_DIFF {nodeId, against}` | 🟢 |
| Changed-path row | click | Loads + shows Monaco before/after for that file | `BLOB_READ` ×2 (parent + node trees) | 🟢 |
| Failing-unit `▸` (Results-detail) | click | Expands unit detail/output | result-read | 🟡 P7 |
| Conversation composer | type/send | — | agent-turn loop | 🟡 P6/P7 |
| "View handoff doc" (Info) | click | — | handoff read | 🟡 P7 |
| Run-history selector (Results) | click | — | run-history read | 🟡 P7 |
| Editable attribution ✎ (Info) | click | — | attribution edit | 🟡 P7 |

### 7.5 Bottom status/run rail & status sub-strip

| Component | Trigger | Visual change | Action / command | Tag |
|---|---|---|---|---|
| Rail tab: **Activity** | click | Shows op-log + outcomes; tab highlighted | Local (op-log stream) | 🟢 |
| Rail tab: Run output | — | (drawn for placement) | streaming `RUN_STDOUT` producer | 🟡 |
| Collapse ⌃ | click | Rail collapses to its tab bar; canvas grows; status sub-strip stays | Local | 🟢 |
| Copy ⧉ / Clear ⌫ | click | Copies/clears the active stream | Local | 🟢 |
| Activity line short-id | click | Selects the referenced node | Local select | 🟢 |
| Status-strip branch | click | Opens branch switcher | Local | 🟢 |
| Status-strip retry (when disconnected) | click | Forces a daemon reconnect attempt (5.12) | Local reconnect | 🟢 |

### 7.6 Modal & settings controls

The fields and confirm buttons inside the §5.10 modals and the §5.13 settings popover, each mapped to the command argument it populates.

| Control | In | Visual change | Populates / action | Tag |
|---|---|---|---|---|
| Name field + **Create branch** | New branch (5.10a) | Validates name; on confirm, ref appears | `BRANCH_FORK.name` | 🟢 |
| Into-ref picker `▾` | Merge (5.10b) | Selects merge target | `BRANCH_MERGE.intoRef` | 🟢 |
| **Merge** | Merge (5.10b) | Clean→Merge node; conflict→resolver opens | `BRANCH_MERGE {intoRef, fromNodeId}` | 🟢 |
| 3-way editors + **Apply & merge** | Conflict resolver (5.10b) | Collects per-file resolution; resubmits | `BRANCH_MERGE.resolution` (+`BLOB_READ` base/ours/theirs) | 🟢 |
| **Restore** (danger) | Restore confirm (5.10c) | Working tree+conversation move | `NODE_RESTORE {nodeId}` | 🟢 |
| Branch field + **Commit** | Commit (5.10d) | Success line w/ branch+SHA | `GIT_EXPORT {nodeId, branch}` | 🟢 |
| Remote field + **Push** (danger) | Push (5.10d) | Success, or denial (5.10e) | `GIT_PUSH {nodeId, remote}` | 🟢 |
| Dry-run checkbox + **Run GC** | GC (5.10g) | Shows reclaimable; live run frees bytes | `GC_RUN {dryRun}` | 🟢 |
| Theme toggle | Settings (5.13) | Re-themes (dark only in v1) | Local | 🟢 |
| Density toggle | Settings (5.13) | Switches spacing scale (Comfortable/Compact) | Local | 🟢 |
| Reset panel sizes | Settings (5.13) | Restores default panel widths | Local | 🟢 |
| Default-model `▾` | Settings (5.13) | Sets default for new nodes | Local `setDefaultModel` | 🟢 |

---

## 8. Non-interactive component catalog (display-only, justified)

Every element that exists purely to *inform*. Each is justified; nothing decorative survives.

| Element | Region | Purpose | Source |
|---|---|---|---|
| App mark "spork" | Top bar | Product identity / home affordance anchor | static |
| Project name | Top bar | Which project's DAG is open | daemon/project |
| Type color stripe | Node card | Encodes node *family* at a glance (principle 4) | `descriptor.color` |
| Type icon | Node card / legend / details header | Encodes node *type* | `descriptor.icon` |
| Status badge | Node card / details header | The single most important per-node fact: effectiveStatus | derived (`status`×`isStale`) |
| Branch chip | Node card / Info | Which line of work the node is on | `branchId` |
| Model chip | Node card / Info | Which model produced it (D5 legibility) | `model` |
| Snapshot dot | Node card / Info | Whether the node is restorable (owns state) | `ownsSnapshot` |
| Edge color | Canvas | Encodes relation type without clutter | `edgeType` |
| Legend counts | Navigator | Distribution of node types in the project | computed from graph |
| Streaming cursor `▌` | Conversation / Run output | Signals live, in-progress output | ephemeral stream presence — 🟡 (no producer until P6) |
| Coverage / metrics readout | Results-detail | The point of an observing node | `ResultEnvelope` — 🟡 P7 (no result-read yet) |
| Status badge (result signal) | Node card / Info | The v1-live pass/fail of a check | `NodeView.status` (🟢) |
| Timestamp | Activity lines | When an action settled | event/local clock |
| Severity icon | Activity lines | Accessible (non-color) success/info/error coding | activity level |
| Daemon status dot | Top bar / status sub-strip | Is the daemon connected (trust/context): connected / reconnecting / disconnected (5.12) | connection state |
| Node count | Status strip | Scale-at-a-glance of the graph | graph size |
| Selection id | Status strip | Persistent "what's selected" context | selection |
| Minimap | Canvas | Orientation in a large graph (§14.3) | view-model |
| Irreversible-effects warning text | Restore / Push modals | Honesty contract (§11.4) — never imply undo of external effects | static + (🟡 effects log) |
| "additive/coming in Pn" notes | various | Honesty contract — marks forward-map surfaces, never fakes them | this doc |

---

## 9. States: empty, loading, busy, error, disabled

A designed state for every surface — replacing today's bare muted sentences and invisible busy state.

| Surface | Empty | Loading | Busy (in-flight) | Error |
|---|---|---|---|---|
| Canvas | Illustration + "No work yet" + **Open project** CTA (5.4) | Skeleton nodes / shimmer | New node renders optimistically (pending→running) | Banner: "Couldn't load graph" + retry |
| Node-Details | "Select a node to inspect it" with a hint icon | Tab-body skeleton | Action button shows spinner + dimmed label | Inline error in the affected tab |
| Changes (v1) | "No changed paths vs baseline" | "Computing diff…" | — | "Couldn't read blob: <path>" |
| Info (v1) | — (always populated for a selection) | field skeleton | — | "Field unavailable" per row |
| Canvas search / ⌘K | — | — | — | "No nodes match …" quiet line (no-results state) |
| Disconnected daemon (5.12) | last-known graph stays visible, read-only | "reconnecting…" banner + pulsing dot | actions greyed | amber banner + **Retry now**; dot → disconnected after timeout |
| Conversation (🟡 P6) | shown only once P6 ships; until then the tab is absent | — | — | — |
| Results-detail (🟡 P7) | result is the canvas **status badge**; detail tab absent until P7 | — | observing child pulses on canvas | failed check → red status badge |
| Run output (🟡) | absent until a streaming producer exists | — | — | — |
| Activity rail (v1) | "No activity yet" | — | live append + auto-scroll | error lines in `--danger` with icon |
| Action (any) | — | — | `[data-busy]` spinner; control disabled during flight; **rolls back on nack** (optimistic UI, §14.4) | red activity line with the reason (e.g., capability denied) |
| Disabled action | — | — | — | faint + tooltip explaining *why* (wrong family, no snapshot, capability needed) |

Optimistic UI (§14.4, 🟢): mutations render immediately with a correlation id and reconcile when the matching op-log event arrives; a nack rolls the optimistic state back and logs a red activity line.

---

## 10. Motion & feedback

Restrained, purposeful motion (principle 6):

- **Selection:** 120ms ease — ring fade-in + 2px elevation lift.
- **Running nodes:** a slow 1.6s pulse on the status badge (the only continuous animation).
- **Graph layout:** ELK incremental; nodes **ease to new positions** (no jump) and unchanged subgraphs hold coordinates (§14.3).
- **Optimistic insert:** new node fades+scales in from 0.96→1.0 (150ms); on nack, fades out.
- **Panels/rail:** 160ms collapse/expand.
- **Toasts/activity:** new activity line slides in 1 row; copy/clear give a 1-frame flash.
- **Respect `prefers-reduced-motion`:** disables pulse and transitions, keeps state changes instant.

---

## 11. Accessibility

- **Focus-visible everywhere:** 2px `--accent` ring on every interactive element (today there is none — a real a11y gap).
- **Never color-only:** status and activity severity always pair color with an **icon/shape** (badges, severity glyphs) for colorblind users.
- **Keyboard:** full tab order; ⌘K palette; arrow-key node navigation on the canvas; Esc closes overlays; Enter activates primary.
- **ARIA roles:** canvas nodes as a navigable list/tree; tabs use `role="tab"`/`tabpanel`; rail uses `role="log"`; modals trap focus and are labelled.
- **Contrast:** body text ≥ 4.5:1, large/UI ≥ 3:1 against its surface (the new ramp is chosen to pass; today's grey-on-grey often fails).
- **Hit targets:** ≥ 28px; tooltips on icon-only controls.

---

## 12. Responsiveness, resizing & density

- **Resizable side panels** with drag handles + min/max, and **collapsible** navigator + details + rail (today the 200px/320px columns are fixed and uncollapsible). Persist sizes locally.
- **Breakpoints:** below a width threshold, the navigator auto-collapses to icons; the details panel can become an overlay drawer. The canvas always keeps the majority of the width (principle 1).
- **Density:** "Comfortable" default; a "Compact" toggle (tighter spacing, 12px text) for power users. This is **🟢** — it is pure local renderer state, lives in Settings (5.13), and does not depend on any unbuilt backing.
- **Selection model:** v1 is **single-select** (click a card → one selection). This is an explicit scope decision, not a gap: two-node comparison is served by the Changes-tab baseline picker (`NODE_DIFF.against`). Canvas **multi-select** (rubber-band, bulk actions) is out of scope for v1 — revisit when bulk operations have a backing command.
- **Window-survival:** because the daemon is separate (§14.1), a window reload restores the same graph and in-flight runs — the UI shows a brief reconnect state (5.12), not a cold start.

---

## 13. Feature ↔ Plan traceability matrix

The honesty gate. Every UI capability ↔ its DESIGN section ↔ its IPC/seam backing ↔ the plan phase ↔ status. **No 🟢 row depends on an unbuilt phase; every 🟡 row names the phase that unlocks it.**

### 13.1 BUILT — live in v1 (backed today)

| UI capability | DESIGN § | Backing (IPC / seam / read) | Phase | Status |
|---|---|---|---|---|
| DAG canvas (L→R, React Flow + ELK) | §14.1–14.3 | `graph_view` read; view-model pipeline | F3-UI | 🟢 |
| Click node → exact state (lazy diff) | §14.5 | `NODE_DIFF` + `BLOB_READ` | F3 | 🟢 |
| Real before/after file diff | §14.5 | `NODE_DIFF` + paired `BLOB_READ` | F3 | 🟢 |
| Node cards: type/status/branch/model/snapshot | §6.2, §7.3, §14.5 | `NodeView` fields + derived effectiveStatus | F2/F3-UI | 🟢 |
| Node-type legend + filter (schema-driven) | §14.5 | `NodeTypeRegistry` descriptors | F2 | 🟢 |
| Branch list / switcher | §6.3 | `graph_view.refs` | F2/F3 | 🟢 |
| Open a project (populate the graph) | §5.1 | `open_project` + `graph_view` | F3 | 🟢 |
| Non-destructive restore (code+convo) | §6.4, §10.3 | `NODE_RESTORE` | F3 | 🟢 |
| Restore irreversible-effects warning | §11.4 | static warning (effects log seam) | F3 | 🟢 |
| Branch / fork (manual) | §6.3 | `BRANCH_FORK`, `REF_CREATE`, `REF_MOVE` | F3 | 🟢 |
| Edge encoding: lineage solid / attachment dotted | §6.3, §6.6 | renders existing `edgeType`s | F3-UI | 🟢 |
| Undo / redo over the graph | A.1, A.6 | `OP_UNDO` / `OP_REDO` | F3 | 🟢 |
| Run observing check (Validate/Stress/Sanity) | §8.1–8.2 | `NODE_RUN_CHECK` (result shows as status badge) | P5 | 🟢 |
| Check result **status badge** (pass/fail) | §7.3, §8.1 | `NodeView.status` + `RESULT_RECORDED` | P5 | 🟢 |
| 3-way branch merge + conflict resolver | §6.5, §19.4, A.4 | `BRANCH_MERGE` + `BLOB_READ` | P5 | 🟢 |
| Commit a node to Git | §10.4 | `GIT_EXPORT` | F3-UI | 🟢 |
| Push a node's branch to a remote | §10.4 | `GIT_PUSH` | F3-UI | 🟢 |
| Capability **denial** surface | §15.1–15.2 | deny-by-default broker (error reply) | F3 | 🟢 |
| Garbage collection (dry-run + live) | §10.4, A.2 | `GC_RUN` | F3 | 🟢 |
| Diff node against arbitrary node | A.1 | `NODE_DIFF {against}` | F3 | 🟢 |
| Global activity log (op-log + outcomes) | §14.4 | `oplog-event` stream | F3/F3-UI | 🟢 |
| Optimistic UI + reconciliation | §14.4 | correlation-id + op-log fold | F3-UI | 🟢 |
| Default-model selector (new nodes) | §12.3 | `modelSelector` stamped on `NODE_CREATE` | F4 | 🟢 |
| Auto-Snapshot (drift) nodes appear; collapse | §6.4, §7.4, §10.2 | drift capture → Snapshot nodes | F3 | 🟢 |
| Command palette, search, zoom/fit/minimap, filters | §14.3 | local renderer state | F3-UI | 🟢 |
| Settings: theme/density/panel-reset/default-model | §14 | local renderer state | F3-UI | 🟢 |
| Context menus (node + branch row) | §14.5 | mirror top-bar/nav actions (gating) | F3-UI | 🟢 |
| Disconnected/reconnecting daemon state | §14.1 | connection state + retry | F3-UI | 🟢 |

### 13.2 FUTURE-PHASE — fully designed in §14, implemented when the phase lands

| UI capability | DESIGN § | Backing needed | Phase | Status |
|---|---|---|---|---|
| **Live conversation stream** (plumbed, unproduced) | §14.4, §5.5 | a provider that emits `CHAT_TOKENS` (channel built; no producer) | P6 | 🟡 |
| **Live run output** (plumbed, unproduced) | §14.4, §5.9 | a runner that streams `RUN_STDOUT` (channel built; no producer) | P7 | 🟡 |
| **Typed result-detail rendering** | §8.1, §14.5 | result-read surface (envelope produced by F4/P5, not exposed) | P7 | 🟡 |
| **Create snapshot/import node from the UI** | §6.2, §10.2 | a capture command (`create_node` needs a real content hash the renderer can't mint; nodes are daemon-produced via drift/agent) | P7 | 🟡 |
| **Auto-branch on work (fork-on-divergence)** + read-only attach | §6.6 | daemon policy in the agent-run / drift-reconcile / checkout paths | P6/P7 | 🟡 |
| Conversation **composer** (send → agent turn) | §12, §13 | provider routing + context compile + agent-turn cmd | P6/P7 | 🟡 |
| Multi-provider routing + hot-swap (real) | §12.1–12.5 | `ModelRouter`, provider adapters | P6 | 🟡 |
| Per-node + per-branch **cost ledger** | §5.4, §12.5 | `CostAccountant` | P6 | 🟡 |
| Privacy class per node; per-node budget | §15.5, §9.3 | router enforcement + budget UI | P6 | 🟡 |
| Quality **gates** / verdicts / baselines / flaky | §8.3 | `GatePolicy`, baselines | P7 | 🟡 |
| **Handoff doc** view (regenerable) | §13.5, §13.7 | `HandoffGenerator` (lineage) + read | P7 | 🟡 |
| Expand-context / SelectionDecision trace | §13.6 | lineage Context Compiler | P7 | 🟡 |
| Persistent/historical transcript viewer | §13.7 | transcript-read (history MCP) | P7 | 🟡 |
| Run-history browse (keep-last-N) | §7.3, §8.2 | run-history read | P7 | 🟡 |
| Editable AttributionRecord UI | §10.2, A.3 | attribution edit surface | P7 | 🟡 |
| Effects-log surface on a node | §11.4 | effects-log read | P7 | 🟡 |
| Lineage/History MCP surfaces | §13.7 | `history-mcp` server | P7 | 🟡 |
| Repo-map view; memory store view/pin | §13.1, §13.6 | repo-map + memory | P7 | 🟡 |
| "Check out this node" (materialize worktree) | §14.5, §11.1 | worktree-checkout IPC cmd (F4 seam exists) | P7/P8 | 🟡 |
| Pin a node / baseline | §8.3, A.2 | pin IPC cmd | P7 | 🟡 |
| Inline capability **grant** | §15.2 | capability-grant IPC cmd | P8 | 🟡 |
| Custom-node marketplace; trust tiers; revoked flag | §9.1–9.3 | SDK + marketplace | P8 | 🟡 |
| Engine-health view (store size, lag, leases) | A.6 | observability reads | P8 | 🟡 |
| Team: bundle export/import; unified team DAG | §19 | `.spork-bundle` graft | P9 | 🟡 |
| Rich card title/summary/diff-stat/stale-reason | §14.5 | additive `graph_view` fields (read-only, no schema change) | F3-UI+ | 🟡 |
| Live co-editing / presence | §19 (deferred) | CRDT event variants | post-P9 | ⛔ deferred |
| Graphical custom-node authoring UI | §9 (deferred) | authoring surface | beyond P8 | ⛔ deferred |

> **Removed from the current UI** (present today but not plan-mappable as live actions): the vague **View**, **Analyze**, and **Metadata** toolbar buttons. "View" duplicates node selection; "Analyze" has no implemented runner; "Metadata" becomes the **Info** tab. None map to a built command, so they go. (Earlier jargon — *Recalibrate / Create DT / Submit DT / Create Process* from the reference image — was already removed.)

---

## 14. Future-phase UI surfaces (designed now, scoped by phase)

This section designs **every** UI surface for the not-yet-built phases to the same depth as the v1 body — wireframes, interactive component → visual → action mappings, and non-interactive justifications — each marked 🟡 with the phase that unlocks it. They are **fully specified but not yet implemented**: the layout in §4–§13 reserves a home for each so nothing has to be re-architected when the phase lands. When a phase completes, its surfaces move here → into the main body and get built (see [§16](#16-keeping-this-document-live-the-phase-completion-ritual)).

> Reading guide: each phase below lists *what unlocks it* (the backing the plan must build), then the surfaces. The DESIGN § citations are authoritative for behavior.

### 14.1 P6 — Multi-provider routing, cost ledger & agent conversations

**Unlocked by P6:** `ModelRouter` resolving `ModelSelector` (`pinned|policy|inheritFromParent`) over OpenAI-compat / local (Ollama/LM Studio) / CLI adapters, with fallback + circuit-breaker; capability negotiation (`json_emulated` tool-calls); the `CostAccountant` (cache-aware: read 0.1×, write 1.25–2×); `PrivacyClass` enforcement; per-node token/USD budget; and — critically — a provider that **emits `CHAT_TOKENS`**, which finally makes the conversation surface real. The **agent-run path also activates auto-branching** (DESIGN §6.6): asking the agent to *change* code from a non-tip node auto-forks a branch (from the tip, it continues); asking for *analyze/plan* attaches a read-only node via a dotted `DERIVED_FROM` edge. (DESIGN §5.4, §6.6, §12.1–12.5, §15.5, §9.3.)

**(a) Model selector → provider-grouped, with cost & capability**
```
┌ Model ▾ ─────────────────────────────────┐
│ ANTHROPIC                                 │   ← grouped by provider (D5)
│   ● Claude Opus 4.8   $15/$75  tools✓     │   ← per-model in/out cost + capability flags
│   ○ Claude Sonnet 4.6 $3/$15   tools✓     │
│ OPENAI                                     │
│   ○ GPT-4o            $5/$15   tools✓     │
│ LOCAL (Ollama)                            │
│   ○ llama3.1:70b      free    tools~json  │   ← json_emulated tool-calling (capability negotiation)
│ ──────────────────────────────────────── │
│ Applies to: ( new nodes ▾ )               │   ← scope: default-for-new vs this node (hot-swap)
└───────────────────────────────────────────┘
```

**(b) Per-node model hot-swap + lossy-projection warning** (Info tab gains a Model control)
```
│ Model   ◆ Opus 4.8   [ Switch model ▾ ]            │  ← mid-session swap = a new turn under the new model
│         ⚠ switching to llama3.1 drops Opus reasoning │  ← lossyProjection warning (§12.3) before confirm
```

**(c) Cost block (Info tab) + per-branch cost ledger**
```
┌ Cost (this node) ───────────────┐   ┌ Branch ledger · feat/login ─────────┐
│ in 1,240 · out 3,110 · cache 8k │   │ 14 nodes · $0.84 total              │
│ $0.0142   (cache saved $0.090)  │   │ cache savings broken out: −$0.31    │  ← read-0.1× economics
└─────────────────────────────────┘   └─────────────────────────────────────┘
```

**(d) Privacy & budget controls (Info tab)**
```
│ Privacy ( any ▾ ) local_only · no_third_party_aggregator · any │ ← router refuses cloud for local_only (§15.5)
│ Budget   [ 50,000 ] tokens / [ $1.00 ]  ⓘ broker-capped │  ← per-node cap (§9.3/§15.2)
```

**(e) Conversation tab — now live** (the §5.5 design activates; composer becomes a real control)
```
┌ Conversation ───────────────────────────┐
│  ┌─ user ────────────────────────────┐  │
│  │ add input validation to login      │  │
│  └────────────────────────────────────┘  │
│  ┌─ assistant · Opus 4.8 ────────────┐  │  ← model attributed per turn
│  │ I'll guard auth.ts… ▌              │  │  ← live CHAT_TOKENS stream + cursor
│  └────────────────────────────────────┘  │
│  ┌──────────────────────────────────┐    │
│  │ Message the agent…         ⏎ Send │    │  ← composer drives an agent turn (creates an Edit node)
│  └──────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

**P6 interactive components**

| Component | Trigger | Visual change | Action / backing | Tag |
|---|---|---|---|---|
| Provider-grouped model item | click | Sets model (new-nodes or this node) | `ModelRouter` (`modelSelector`) | 🟡 P6 |
| Switch-model (per node) | click → pick | Confirms; lossy-projection warning if reasoning dropped | hot-swap turn | 🟡 P6 |
| Privacy class `▾` | change | Re-scopes routing; cloud disabled for `local_only` | `PrivacyClass` enforcement | 🟡 P6 |
| Budget field | edit | Sets per-node cap; broker enforces | `model.invoke` budget | 🟡 P6 |
| Conversation composer + Send | type / ⏎ | Streams tokens; spawns an Edit node | agent-turn loop | 🟡 P6 |

**P6 non-interactive:** per-model cost/capability readouts (cost ledger); per-turn model attribution; cache-savings figure; lossy-projection warning; streaming cursor.

### 14.2 P7 — Gates, scheduler, context/handoff & history

**Unlocked by P7:** declarative `GatePolicy` (over `merge|promote|submit-dt|create-dt`) with immutable `GateVerdict`, baselines (correctness + perf), `flakinessScore`/quorum/quarantine, override→audit; the full constraint scheduler (disjoint→parallel else serialize-with-reason, auto-port + per-branch ephemeral DB); the lineage-aware Context Compiler + auto handoff docs + memory + repo-map; the read-only Lineage/History MCP; a **result-read surface** (makes the §5.7 Results-detail real); a **streaming `RUN_STDOUT`** producer (makes the §5.9 Run-output tab real); and **"check out this node"** which, with drift capture, completes auto-branching for *human* edits to a historical node (fork-on-divergence, DESIGN §6.6). (DESIGN §8.3, §11.2, §13, §13.7, §6.6.)

**(a) Gate verdict badge + override-with-audit** (on a transition / the Merge modal)
```
┌ Merge gate ─────────────────────────────┐
│ ✓ tests pass   ✕ coverage 78% < 80% gate │  ← GateVerdict (immutable, travels w/ snapshot)
│ ⚠ Merge BLOCKED by policy "main-protect"  │
│ [ Cancel ]  [ Override → records audit node ] │  ← override is never silent (§8.3)
└─────────────────────────────────────────┘
```

**(b) Baselines & diff-vs-baseline** (Results-detail + a baseline picker)
```
│ Baseline: [ v1.2-green ▾ ]  [ Pin current as baseline ] │
│ vs baseline:  +2 newly-passing   −1 regressed   ~ p95 +12ms │  ← Diff engine deltas (§8.3)
│ Flaky: 1 quarantined (time-boxed)   flakiness 0.34          │
```

**(c) Scheduler "queued with reason" node state** (canvas card + Info)
```
│ ◷ queued — conflict on db:postgres-primary (serialized) │  ← honest serialize-with-reason (§11.2)
```

**(d) Handoff document viewer** (new Node-Details tab)
```
┌ Handoff ────────────────────────────────┐
│ Summary · Decisions · Files+rationale     │  ← auto-generated at completion + branch points
│ Open threads · Constraints · Test state   │  ← regenerable; exportable as AGENTS.md
│                         [ Regenerate ] [ Export ] │
└─────────────────────────────────────────┘
```

**(e) Context trace ("why this context")** (new Node-Details tab / expander)
```
│ ▸ auth.ts          included (changed in lineage)     │  ← SelectionDecision trace (§13.6):
│ ▸ legacy/*         summarized (budget)               │     one honest explanation, not knob soup
│ ▸ node …9F8E       dropped (low rank)                │
```

**(f) History / Lineage surfaces** (a search panel + the MCP that also drives external agents)
```
┌ History ⌕ "why did we drop redis" ──────┐
│ ◆ …7A2 find_decisions → "switched to pg" │  ← search_history / find_decisions / walk_ancestors
│ ◆ …5C4 files touched: cache.ts, db.ts    │  ← read-only, lineage-scoped (nodes.readOutputs)
└─────────────────────────────────────────┘
```

**(g) Activated v1-deferred surfaces:** the **Results-detail tab** (§5.7), **run-history** browser, and the **Run-output rail tab** (§5.9 streamed `RUN_STDOUT`) all light up here. Node context-menu **Check out this node** and **Pin** become live; Info gains an **editable Attribution** control and an **Effects-log** surface (external writes recorded, §11.4); memory store (project/lineage/node, with TTL + pinning) and the repo-map view appear.

**P7 interactive components**

| Component | Trigger | Visual change | Action / backing | Tag |
|---|---|---|---|---|
| Gate override → audit | click | Records an audit node; transition proceeds | `GatePolicy` override | 🟡 P7 |
| Pin as baseline / Pin node | click | Pin badge; protected from GC/eviction | baseline/pin surface | 🟡 P7 |
| Result-detail expanders / run-history | click | Expand units; switch runs | result-read | 🟡 P7 |
| Run-output rail tab | click | Live `RUN_STDOUT` stream (auto-scroll) | streaming producer | 🟡 P7 |
| Handoff: Regenerate / Export | click | Rebuilds / exports AGENTS.md | `HandoffGenerator` | 🟡 P7 |
| Check out this node | context menu | Materializes a worktree (labelled slow) | worktree-checkout cmd | 🟡 P7 |
| History query | type | Lineage-scoped results | History MCP (read-only) | 🟡 P7 |
| Editable attribution ✎ | click | Corrects agent/human attribution | attribution edit | 🟡 P7 |

**P7 non-interactive:** gate verdict readout; baseline deltas; flakiness/quarantine; queued-with-reason; context-selection trace; effects-log entries; memory provenance/TTL; repo-map.

### 14.3 P8 — Custom-node SDK, tiered executors & marketplace

**Unlocked by P8:** the Custom Node SDK + `.spork-node` package (manifest + executor + sandboxed UI bundle + lockfile + SBOM + signature + trust tier); a signed marketplace with the **Verified / Community / Local-Dev** trust ladder; the revocation kill-switch (retain-and-flag `revoked_provenance`); tiered isolation backends (WASM/WASI P2, OCI container, microVM); the engine-health observability reads; and an inline capability-grant command. (DESIGN §9.1–9.3, §11.1, A.6.)

**(a) Marketplace / Extensions (new top-level view)**
```
┌ Extensions ⌕ ───────────────────────────────────────────────┐
│ INSTALLED                                                     │
│   ✓ mutation-test   Verified Publisher        [ Manage ]      │  ← trust-tier badge per item
│   ⚠ perf-fuzz       Community · provenance REVOKED [ Remove ] │  ← retain-and-flag revocation
│ BROWSE                                                        │
│   ◆ contract-test   Local-Dev    [ Install… ]                 │
└──────────────────────────────────────────────────────────────┘
```

**(b) Capability-request review (before install)** — every requested capability shown
```
┌ Install "contract-test"? ───────────────┐
│ Requests:                                │
│   ● process.spawn   run the test binary  │  ← human-readable rationale per capability (§9.2)
│   ● snapshot.read   read changed files   │
│   ○ net.connect     (not requested)      │
│ Trust: Local-Dev (unsigned)  ⚠           │
│              [ Cancel ] [ Grant & install ] │
└─────────────────────────────────────────┘
```

**(c) Isolation-tier selector (per node-type / check)**
```
│ Run in:  ( WASM ● )  Container   microVM   │  ← tiered executors (§11.1); deny clock/random/net by default
```

**(d) Engine health (Settings → Engine health)**
```
┌ Engine health ──────────────────────────┐
│ Object store 1.8 GB · orphans 142        │  ← A.6 observability
│ Projection lag 0 · leases 3 active       │
│ [ Run GC… ]                              │
└─────────────────────────────────────────┘
```

**(e) Inline capability grant** — the §5.10e denied modal gains a one-click **Grant** (capability-grant command); revoked-provenance badge decorates affected node cards; custom-node UI renders in a sandboxed (postMessage-only) webview.

**P8 interactive components**

| Component | Trigger | Visual change | Action / backing | Tag |
|---|---|---|---|---|
| Install / Manage / Remove extension | click | Updates installed set; capability review first | marketplace + SDK | 🟡 P8 |
| Grant & install | click | Grants reviewed capabilities; installs | capability-grant cmd | 🟡 P8 |
| Isolation-tier selector | change | Sets executor tier for runs | `IsolationBackend` impls | 🟡 P8 |
| Inline "Grant" (denied modal) | click | Enables the gated action | capability-grant cmd | 🟡 P8 |

**P8 non-interactive:** trust-tier badges; revoked-provenance warnings; engine-health metrics; SBOM/signature provenance.

### 14.4 P9 — Shared-team bundle grafting

**Unlocked by P9:** `.spork-bundle` export/import (set-difference of objects + typed nodes/edges + tip refs; import grafts a sub-DAG, shared ancestors dedup by hash); pluggable transports (local-file / sync-server / git-remote / s3 behind one have/want contract; git-remote maps `GateVerdict`s → external Git status checks); the unified team DAG with `created_by`; and team merge = subgraph grafting + 3-way reconciliation (observing results re-run post-graft; conversation merge via synthetic-transcript). (DESIGN §19.1–19.6.)

**(a) Team / Remotes (navigator section) + unified DAG attribution**
```
┌ TEAM / REMOTES        + ⌃ │      Canvas cards gain author:
│  ⤓ origin (git-remote)    │        ┌──────────────┐
│  ⤓ team-sync (sync-server)│        │ ✎ Edit  @ana │  ← created_by avatar; filter by author
│  Filter by author: ( all ▾)│       └──────────────┘
└────────────────────────────┘
```

**(b) Export / import bundle**
```
┌ Export bundle ──────────────────────────┐   ┌ Import bundle ──────────────────────────┐
│ Sub-DAG: from ✎…A1B2 (14 nodes)          │   │ Grafting 9 nodes onto main…             │
│ Transport: ( local file ▾ )              │   │ shared ancestors dedup by hash (−6)     │  ← have/want
│ git-remote → GateVerdicts as checks      │   │ ⚠ 1 conflict → 3-way resolver (reuse)   │  ← reuses §5.10b
│               [ Cancel ] [ Export ]      │   │              [ Cancel ] [ Graft ]       │
└─────────────────────────────────────────┘   └─────────────────────────────────────────┘
```

**P9 interactive components**

| Component | Trigger | Visual change | Action / backing | Tag |
|---|---|---|---|---|
| Export bundle | click | Writes `.spork-bundle` via chosen transport | bundle export | 🟡 P9 |
| Import / Graft | click | Grafts sub-DAG; conflicts → 3-way resolver | bundle graft | 🟡 P9 |
| Author filter | change | Dims non-matching authors' nodes | local + `created_by` | 🟡 P9 |
| Transport config | edit | Adds/edits a remote | transport adapter | 🟡 P9 |

**P9 non-interactive:** `created_by` avatars; dedup-by-hash counts; per-remote transport status; gate-as-check mapping.

### 14.5 Deferred (post-P9) — designed lightly, intentionally not scheduled

These are **explicit out-of-scope** items (plan §11), designed only enough to show they fit the same surfaces:

- **Live co-editing / presence (CRDT)** — would add presence cursors/avatars on shared nodes and live-edit indicators; admitted later as additive op-log event variants. *Not* in any P-phase.
- **Graphical custom-node authoring UI** — a visual descriptor/editor builder writing into the frozen F2 registry; until then, authoring is the P8 SDK/CLI.

---

## 15. Open decisions to confirm

Before the UI is rewritten, please confirm (defaults in **bold** are my recommendation):

1. **Scope discipline (living document):** the doc designs **all phases** (§14, fully wireframed); the v1 *build* implements **only 🟢 BUILT features** (no dead "coming soon" controls). As each phase lands, its surfaces are re-tagged 🟢, moved into the main body, and then built — per the §16 phase-completion ritual. — *Recommended.*
2. **Theme:** **dark-first now**, tokenized so light is a later swap — vs. shipping light in parallel. — *Recommended: dark-first.*
3. **Accent identity:** **iris/indigo `#7c8cff`** as the single accent — vs. a different hue (teal, violet, emerald). Easy to change (one token).
4. **Action toolbar placement:** keep the node-action toolbar in the **top bar** (single instance, §14.2-faithful) — vs. a floating toolbar attached to the selected node. — *Recommended: top bar.*
5. **Honest v1 narrowing (important):** confirm that v1 ships **without** a live conversation, live run-output stream, or detailed result panel — because nothing produces `CHAT_TOKENS`/`RUN_STDOUT` and no command reads the `ResultEnvelope` yet (verified against the code). In v1, **check results appear as the canvas status badge**; the Conversation tab arrives in P6 and the Results-detail/Run-output surfaces in P7. The v1 Node-Details tabs are **Changes + Info**. — *This is the honesty contract in action; please confirm you're OK with the narrower-but-truthful v1.*
6. **Selection model:** v1 is **single-select** (two-node compare via the diff baseline picker); canvas multi-select is out of scope for v1. — *Recommended.*
7. **Icon set & fonts:** **Lucide** icons + **Inter/JetBrains Mono** (all OSS) — vs. alternatives.
8. **Rich-card fields:** approve adding **read-only denormalized fields** (title, diff-stat, stale-reason) to the `graph_view` projection so cards are information-rich (additive, no schema/log change).

Once confirmed, the rewrite replaces `app/src/**` per this document, keeping the frozen IPC surface untouched, and `TODO.md` / the doc index get an F3-UI-redesign entry.

---

## 16. Keeping this document live (the phase-completion ritual)

This document is **living**. It always describes the *whole* product UI/UX (every phase, §14 included); what changes over time is which surfaces are *implemented*. The rule:

> **A core-functionality change must be paired with a UI/UX-document change — and then the corresponding UI work.** New backend capability and its on-screen surface are one unit of work, never two.

**When a phase (P6, P7, P8, P9) completes — do this, in order:**

1. **Re-scope the doc.** For each surface designed under that phase in §14, change its tag **🟡 → 🟢** and **move it from §14 into the main body** (the relevant region in §4–§9 and the matrix in §13.1). Delete the now-empty §14 entry. The phase's `RUN_STDOUT`/`CHAT_TOKENS`/result-read producers, etc., that were "plumbed-but-unproduced" become "produced" — update the §2 note accordingly.
2. **Then implement.** Build those surfaces in `app/src/**` against the now-real backing, keeping the frozen IPC contract intact (new commands are additive, C2/C3).
3. **Verify the honesty contract still holds.** Every 🟢 element must trace to a backing that genuinely exists (run the §13 matrix check); no surface advertises a capability the daemon can't fulfil.
4. **Record it.** Tick the phase's UI items in `TODO.md`, note the doc re-scope in the commit, and update this section's changelog below.

**When a *mid-phase* core change lands** (a new command, a new view-model field, a new event): before/with that change, add or update the matching UI/UX entry here (even if still 🟡), so the document never lags the backend. If a backend change has **no** UI consequence, say so explicitly in the PR ("no UI surface") so the omission is a decision, not an oversight.

**Re-scope changelog** (append one line per phase as it lands):

| Date | Phase | Surfaces promoted 🟡→🟢 | Notes |
|---|---|---|---|
| _(pending)_ | v1 / F3-UI | the §13.1 set | initial implemented surface |
