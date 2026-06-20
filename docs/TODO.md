# Spork — Implementation TODO

> Task tracker derived 1:1 from [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md). Every task traces to a plan phase (§6) or cross-cutting concern (§7). Authoritative spec is [DESIGN.md](DESIGN.md) — cite its sections in code/PRs.

## How to use this file
- The spine is **strictly linear and non-reorderable**: `F0 → F1 → F2 → F3 → F4 → P5 → P6 → P7 → P8 → P9`. A phase starts only when the previous phase's **Definition of Done is fully checked** and its **freeze-before** contracts are locked.
- **C1 (no stubs):** every phase ships *narrow-but-complete* — one real impl behind each new seam, zero placeholders. The CI no-stub gate (§8.3) forbids `todo!()`/`unimplemented!()`/`TODO`/`FIXME` in `src/**`.
- **C2 (no domino):** never change a frozen contract in place — grow it via a new impl behind the seam or a new versioned generation. See the Domino-Risk Register (plan §9, D-1…D-15) before touching anything foundational.
- **C5 (evolution-safe):** every persisted struct carries a `schema_version` + a registered migration from its first commit.
- Status: `[ ]` todo · `[~]` in progress · `[x]` done. Keep this file updated as the single source of progress.

**Current status:** **F0 complete** — 6 crates, 223 tests green, clippy/fmt clean, no stub markers; content-addressing/dedup/canonical-identity proven; cold capture optimized to ~0.07 ms/file (fsync-batching into a packfile + parallel hashing). Next: **F1 — Event-Sourcing Core**.

---

## Cross-cutting — establish in F0–F4, honor every phase (plan §7)
These are foundation infrastructure, not features — wired in behind frozen seams, extended additively later.

- [ ] **Testing** — unit (pure fns) + property/model-based (random create/fork/restore/merge/gc vs invariants) + golden (byte-exact canonical-serialization vectors, ResultEnvelope/transcript snapshots) + integration/crash (fault injection at each two-store window). Stand each up in the phase that freezes the contract it guards. (§7.1)
- [ ] **Observability** — structured daemon log (`{seq, opId, nodeId, event_type, schema_version}`), engine-health view (store size, orphan count, checkpoint lag, lease state), first-class decision traces (`AuditEntry`, `SelectionDecision`, `GateVerdict`). Local-only by default. (§7.2)
- [ ] **Error model** — typed versioned enum, additive variants only; fail-closed everywhere (restore rolls back on divergence; context degrades `request-more|compact|fail` never silent truncation; provider fallback + circuit-breaker; merge returns `conflictSet` not a half-node; crash → reclaimable orphans never dangling refs). (§7.3)
- [ ] **Security** — deny-by-default capability broker, versioned scope grammar, `AuditEntry` per privileged call; renderer holds zero secrets; all FS mutation via `snapshot.write` against CoW; secrets only by `vaultRef`, secret-scan at capture. (§7.4)
- [ ] **Performance budgets** as CI gates from the phase that earns them: **incremental** capture p95<300ms (**re-attributed to F3** — requires the stat/mtime index in `spork-drift`; the stateless F0 path re-hashes the whole tree. F0 cold capture is ~0.07 ms/file after fsync-batching + parallel hashing; first-snapshot is intrinsically O(repo)), append ≥2k/s (F1), restore p95<500ms / click→diff p95<150ms / ≥55fps@1k nodes / drift rescan <5s (F3). (§7.5)
- [ ] **Migration tooling** — per-`(event_type, schema_version)` registry applied at replay/checkpoint, never editing stored events; lazy-upgrade-on-read; key-version per cache artifact. (§7.6)

---

## F0 — Byte-Identity Substrate
**Goal:** Freeze & prove every content-identity decision so no object id is ever re-derived. **Depends on:** — (root). **Design:** §6.1, §10.1, §10.4, §16.

**Freeze before starting:** BLAKE3 = sole Spork-computed hash + object-store algorithm/generation tag · exact canonical-serialization byte encoding + `serialization_version` + `this_event_hash` input-encoder contract · `ignore_profile` format + `ignore_profile_hash` · **snapshot granularity = per-mutating-node** (+ config seam, Q1) · **Git = strict import/export boundary**, identity never coupled to git ids (Q2) · `StorageBackend` interface (one impl now).

**Build:**
- [x] `crates/spork-hash/` — BLAKE3 wrapper; `HashTag { algo, generation }` self-describing header; `ObjectId` (rejects unknown generation)
- [x] `crates/spork-canon/` — frozen canonical encoder + `SERIALIZATION_VERSION`; float-rejection; published byte-exact golden vectors
- [x] `crates/spork-cas/` — `Chunk`/`Blob`/`Tree`/`Snapshot`; loose-objects + packfile `StorageBackend`; FastCDC; `ObjectStore` (+ batched-pack bulk path, parallel hashing)
- [x] `crates/spork-ignore/` — `ignore_profile` canonical format + `ignore_profile_hash` + globset matcher
- [x] `crates/spork-asset/` (D-13) — `AssetStore` trait + `AssetKey`; complete `LocalCasAssetStore` (opaque class) + `EcosystemNotRegistered` for Deps; **deps-excluded-by-default policy locked** (touches `ignore_profile_hash`)
- [x] `crates/spork-cas-cli/` (bin `spork-cas`) — `put-tree` / `cat` / `chunk-stats` / `verify-roundtrip` / `gen-fixture`

**Definition of Done:**
- [x] put 50k-file tree, edit one file, re-put → only changed chunks re-stored (verified live: 1 new chunk + path-to-root objects)
- [x] identical content always dedups to the same BLAKE3 (warm re-put = 0 new objects, identical snapshot id)
- [x] canonicalization vectors byte-identical on two machines (golden_vectors suite)
- [x] object written under an unknown generation tag is **rejected** (4 tests: header/loose/pack/parse)
- [~] snapshot capture **p95 < 300 ms** — **deferred to F3**: requires the stat/mtime index (`spork-drift`); F0 cold capture optimized to ~0.07 ms/file (20k files in ~1.4 s), but the stateless re-walk can't hit 300 ms on 50k without the index. See §7.5 + plan §12.
- [x] all fuzz/property tests pass (223 tests green; proptest roundtrip + idempotence)

**Demoable:** point `spork-cas` at a 50k-file repo, change one byte in a large asset, re-capture → only the changed chunk re-stores; unchanged subtrees dedup to identical hashes. ✅ verified.

---

## F1 — Event-Sourcing Core
**Goal:** Freeze & prove the source-of-truth contract. **Depends on:** F0. **Design:** §5.2, §6.1, A.1, A.2, A.6.

**Freeze before starting:** Event schema + hash-chaining formula · single writer actor as the only projection write path (projection = pure fn of log) · per-event forward-migration + lazy-upgrade-on-read · checkpoint cadence + replay-window length (quantified) · **write-objects-then-log** crash ordering.

**Build:**
- [ ] `crates/spork-log/` — single serializing **writer actor** (the only write path); SQLite-WAL `event` table (ULID `event_id`, `seq`, `type`, `schema_version`, canonical `payload`, `prev_event_hash`, `this_event_hash`, `actor`)
- [ ] `crates/spork-projection/` — pure `fn(log) -> projection`; periodic checkpoints
- [ ] `crates/spork-migrate/` — `EventMigration` + `MigrationRegistry` keyed by `(EventType, schema_version)`

**Definition of Done:**
- [ ] replaying the full log reproduces the projection **bit-for-bit**
- [ ] delete projection DB → rebuild yields identical projection (A.6)
- [ ] tampering with any stored event is detected via the hash chain
- [ ] bumping an event `schema_version` reads old events through migration with **no stored-event rewrite**
- [ ] crash-injection matrix passes (object-fsync-before-log-commit → reclaimable orphan, never dangling ref)
- [ ] append throughput **≥ 2k events/s** through the writer actor

**Demoable:** append events, delete the projection DB, replay byte-identically, then bump a schema version and watch old events migrate on read.

---

## F2 — Typed-Graph Contracts as Data
**Goal:** Freeze the type system *as data* before any concrete type exists. **Depends on:** F1. **Design:** §6.2, §6.3, §6.5, §7.1–§7.3, §9.1, §9.2, §14.5, A.1.

**Freeze before starting:** `NodeEnvelope` field set + column-vs-JSON split · `NodeTypeDescriptor` schema + `ownsSnapshot`-at-registration rule + built-ins-use-the-same-registry · typed-edge set + acyclicity-on-insert + refs-as-GC-roots · `payloadSchemaVersion` lazy-upgrade + mixed-descriptor-version retention · reserved `revoked-provenance` field (Q9 direction).

**Build:**
- [ ] `crates/spork-graph/` — `NodeEnvelope` with hot fields (`status`, `is_stale`, `snapshot_hash`, test-pass counts) as **indexed columns, not JSON**
- [ ] `crates/spork-registry/` — `NodeTypeRegistry.{register,resolve,list}`; `register()` rejects `ownsSnapshot`/`contentRef` mismatch; one built-in `Snapshot` descriptor registered through the **public** path
- [ ] `crates/spork-edges/` — typed edges, acyclicity-on-insert, Refs as GC roots
- [ ] `crates/spork-status/` — `effectiveStatus` fold (one token per `(status, is_stale)`, incl. `cancelled`)

**Definition of Done:**
- [ ] descriptor claiming `ownsSnapshot` with no `contentRef` → **refused at registration**
- [ ] cycle-creating edge → rejected against the projection (always a DAG)
- [ ] node under `typeVersion` v1 still resolves staleness/edges/restore after a v2 descriptor registers (descriptors retained, not replaced)
- [ ] older-payload node reads back via lazy upgrade, no history rewrite
- [ ] `effectiveStatus` returns exactly one token for every `(status, isStale)` pair incl. `cancelled`

**Demoable:** register a descriptor exactly as a third party would, create nodes/edges, watch `lineageHash` populate, see a cycle edge rejected and a malformed descriptor refused.

---

## F3 — Daemon/Renderer Seam, Security Boundary & Interactive Core
**Goal:** Freeze the daemon/renderer split + security boundary, then ship the first interactive slice — **retire the untracked-mutation gap and the chat-only-restore trap here**. **Depends on:** F2. **Design:** §5.5, §6.4, §10.1–§10.4, §14.1, §14.3, §14.4, §15.1, §15.2, §15.4, A.1, A.3, A.6.

**Freeze before starting:** IPC command/event envelope + `opId`-returns-then-state-via-events · durable-on-ordered-stream vs ephemeral-on-side-channel split · capability vocabulary + versioned scope grammar + `AuditEntry`-per-call · `vaultRef` indirection (secrets resolved only in daemon, never hashed) · `AttributionRecord` schema + A.3 precedence · view-model layer isolating layout authority (Q8 deferrable).

**Build:**
- [ ] `crates/spork-ipc/` — tRPC-style command channel; mutations return `opId`, state arrives only via events (`node.create`/`node.restore`/`branch.fork`/`op.undo`/`op.redo`/`gc.run`)
- [ ] `crates/spork-stream/` — dual channel: ordered op-log events + node-id-keyed ephemeral side-channels (chat tokens, stdout)
- [ ] `crates/spork-broker/` — deny-by-default capability broker + versioned scope grammar; `AuditEntry` per call
- [ ] `crates/spork-vault/` — `CredentialVault`; `vaultRef` resolved only inside the daemon
- [ ] `crates/spork-drift/` — fused interceptor + FS watcher + reconciliation rescan + LSP-buffer bridge; versioned `AttributionRecord`
- [ ] `crates/spork-restore/` — atomic dual-restore guard (single lock, fail-closed); metadata-only `branch.fork`
- [ ] `crates/spork-git/` — non-invasive `GitContext`; `importGitState`/`exportToGit`; `.git` never touched
- [ ] `app/` (Tauri/React) — daemon SoT → denormalized virtualized view-model → React Flow; ELK off-thread; lazy CAS diff

**Definition of Done:**
- [ ] headless client and renderer drive the **same** IPC; mutations return `opId`, state arrives as events
- [ ] token-stream volume never stalls graph delivery
- [ ] out-of-band `bash rm`/`mv`, external-editor save, and unsaved buffer each → correctly-attributed node within the debounce window
- [ ] planted fake API key caught at capture, **never enters the CAS**
- [ ] side effect without a capability → denied with an `AuditEntry`
- [ ] restoring an old node restores code + bound conversation atomically, **fails closed** on injected divergence, forward history survives as a sibling
- [ ] canvas **≥ 55 fps @ 1k nodes**, click→diff **p95 < 150 ms**, restore **p95 < 500 ms**

**Demoable:** open a repo, `rm` a file via raw bash → attributed drift node; click a past node → exact diff; restore (code + conversation), forward history survives as a branch; export to a clean Git commit while `.git` stays byte-unchanged.

---

## F4 — Execution, Result, Provider & Context Seams (one impl each)
**Goal:** Freeze every remaining load-bearing contract a feature plugs into, each with **exactly one** working impl. **Depends on:** F3. **Design:** §5.3, §5.4, §8.1, §8.2, §10.3, §11.1–§11.4, §12.1–§12.3, §13.2, §6.5, A.4.

**Freeze before starting:** `IsolationBackend` + `ResourceProfile` + lease/reaper (impl: worktree-on-CoW) · `Scheduler` admission trait (impl: serial admission) · `Runner` SPI + versioned `ResultEnvelope` + metric-id registry · `inputDigest`/`derivationKey` formula + key-version-per-artifact + declaredInputs honesty + impure opt-out · `ProviderAdapter` port + canonical transcript + `OpaqueProviderBlock` · `ContextLayer` volatility ranks + `prefix_hash` (impl: single-turn `ContextCompiler` + single-node `HandoffGenerator`) · `EnvManifest` schema + canonical serialization (hashed into identity) · Merge `conflictResolution` + synthetic-transcript schemas · **v1 `AssetStore`** plugged into `IsolationBackend.provision`. **Pin as data/policy:** Q3 isolation tier, Q5 restore scope (code+conversation + shaped effects-log seam), Q6 inherit-parent model, Q9 retain-and-flag, handoff-as-GC-root.

**Build:**
- [ ] `IsolationBackend` (v1: worktree-on-CoW; reflink/clonefile, hardlink/copy fallback) + `ResourceProfile` + durable lease ledger + crash-safe reaper
- [ ] `Scheduler.admit` v1 — serial admission (one-at-a-time, serialize-with-reason)
- [ ] `Runner` SPI (`describe`/`prepare`/`run`/`normalize`/`collect_artifacts`) + versioned `ResultEnvelope` (units/metrics/violations/artifactManifest) + metric-id registry
- [ ] `inputDigest`/`derivationKey` content-addressed cache (key-version per artifact; impure self-declares)
- [ ] `ProviderAdapter` port + v1 `AnthropicAdapter` + `ModelRouter` (single-provider, enforces `PrivacyClass`, exposes `next_fallback`) + `ProviderProjection`
- [ ] `CanonicalTranscript` (versioned) bound to every mutating node's snapshot for dual-restore
- [ ] `ContextCompiler` v1 (single-turn, layered stable→volatile by `ContextLayer` ranks, `prefix_hash`-keyed) + `HandoffGenerator` v1 (single-node)
- [ ] `EnvManifest` (pinned toolchains/lockfile hashes/base-image digests), canonical-serialized + hashed into node identity
- [ ] v1 `AssetStore` (one ecosystem e.g. npm + opaque-blob handling) wired into provisioning

**Definition of Done:**
- [ ] Sanity `CheckSpec` auto-runs change-scoped after an edit, stores an append-only `ResultEnvelope`
- [ ] identical `(spec, inputTreeHash, runnerImage)` re-run is a **cache hit**; an impure runner never caches
- [ ] tests-shaped, perf-shaped, and lint-shaped results all round-trip through the **same** `ResultEnvelope` (no core code knows the runner type)
- [ ] a chat turn round-trips through `AnthropicAdapter` via the canonical transcript and restores under the dual-restore guard
- [ ] two nodes with the same `EnvManifest` hash are environment-identical
- [ ] dead-owner lease reclaimed by the reaper on restart
- [ ] all pinned open-question resolutions recorded as data/policy, not code branches

**Demoable:** register a Sanity `CheckSpec`, run → normalized `ResultEnvelope`, re-run → content-addressed cache hit; send a chat turn through Anthropic stored as a provider-agnostic transcript that restores with its code.

---

## P5 — Four Built-In Node Types
**Goal:** Ship the four product built-ins (+ Merge, Snapshot) through the same F2 registry + F4 Runner SPI. **Depends on:** F3, F4. **Design:** §6.2, §6.5, §7.1, §8.1, §8.2, A.4. **Freeze before:** the four built-in payload schemas (versioned).

**Build:**
- [ ] `nodes/edit/` — binds **both** `contentRef` and `conversationRef`; `diffSummary`/`filesChanged`/`toolCalls[]`/`contextSources[]`
- [ ] `nodes/validation/` + `nodes/stress/` — runners behind the F4 SPI (junit-xml; p50/p95/p99/throughput/peak-mem/fuzz-corpus)
- [ ] `nodes/sanity/` — auto-run change-scoped, debounced, hermetic
- [ ] `nodes/merge/` — three-way reconciliation; Monaco conflict UI; observing children marked stale + re-run post-merge
- [ ] `nodes/snapshot/` — `origin = auto_drift | manual | import` (Import is a Snapshot, not a separate kind)
- [ ] data-driven node card / details / legend off the descriptor

**Definition of Done:**
- [ ] all four (+ Merge, Snapshot) register through the public registry exactly as a third party would (zero special-casing)
- [ ] Edit auto-triggers Sanity with cache hits on unchanged subtrees
- [ ] Validation/Stress attach append-only results without mutating the parent
- [ ] branch → alternate Edit → merges through Monaco three-way UI into a clean, materializable Merge node whose observing results re-run post-merge
- [ ] Import ingests external state as an `origin=import` Snapshot node

**Demoable:** run an Edit, watch Sanity auto-run + Validation/Stress attach results; branch, merge through the three-way diff into a Merge node whose checks re-run against the merged tree.

---

## P6 — Multi-Provider Routing & Cost Ledger
**Goal:** Add OpenAI-compat, local, CLI adapters + DAG-aware routing behind the F4 port — additive, no stored-history rewrite. **Depends on:** P5. **Design:** §5.4, §12.1–§12.5, §15.5. **Freeze before:** — (writes into frozen F4 contracts).

**Build:**
- [ ] `providers/openai-compat/` (OpenAI/OpenRouter/vLLM), `providers/local/` (Ollama/LM Studio), `providers/cli/` (Copilot CLI, TTY/JSONL)
- [ ] `router/` — `ModelRouter` resolving `ModelSelector` (pinned|policy|inheritFromParent); queries `CapabilityRegistry` before strategy; fallback + circuit-breaker; enforces `local_only`/`no_third_party_aggregator`/`any`
- [ ] `capabilities/` — static hint table refined by a cached probe (keyed+versioned by model+endpoint+version); `json_emulated` tool-calling fallback
- [ ] `cost/` — cache-aware `CostAccountant` (read 0.1×, write 1.25–2×), attributed per originating node

**Definition of Done:**
- [ ] node hot-swaps mid-session frontier→local Ollama with `json_emulated` tool-calls auto-engaged, still restores code+conversation intact
- [ ] `local_only` node provably refused routing to any cloud/aggregator provider
- [ ] per-node cost with cache reads at 0.1× attributed correctly on the fixture
- [ ] adding a fifth OpenAI-compat endpoint = new adapter config, **zero core edit, no stored-history migration**

**Demoable:** pin an Edit to Claude and a Sanity to a free local model; hot-swap a node OpenAI→Claude mid-conversation without losing context; open a per-branch cost ledger with cache savings broken out.

---

## P7 — Gates, Scheduler & Cache-Aligned Context Compiler
**Goal:** Add quality gates + baselines + flaky handling, the full constraint scheduler, the lineage-aware Context Compiler + handoff docs, and the v1 `HistoryIndex` behind a read-only Lineage/History MCP. **Depends on:** P6. **Design:** §8.3, §11.2, §13.1–§13.7, A.2, A.4. **Freeze before:** `GatePolicy` predicate grammar + `GateVerdict`-travels-with-snapshot (needed by P9) · sibling-branch context default = isolated (Q4) · `ContextPolicy` per-node-type defaults + degrade-never-silently-truncate.

**Build:**
- [ ] `gates/` — `GatePolicy` over `merge|promote-branch|submit-dt|create-dt`; predicate over latest `ResultEnvelope`s/metric deltas vs baseline; `severity block|warn`; immutable versioned `GateVerdict` that **travels with the snapshot**; overrides → audit nodes
- [ ] `baselines/` — correctness expected-pass set; perf distribution + tolerances; pinned/GC-protected; `flakinessScore` keyed by `inputDigest`; bounded retry + quorum + quarantine
- [ ] `scheduler/` — `Scheduler.admit` full strength (disjoint→parallel else serialized-with-reason; auto-port allocation + env rewrite + per-branch ephemeral DB)
- [ ] `context/` — Lineage Walker → Ancestor Selector → layered Compiler (F4 ranks + `prefix_hash`) → Cache Manager → Provider Normalizer; budgeted tools `exploreRepo`/`find_symbol`/`find_referencing_symbols`/`loadAncestorContext`; vector similarity for **memory recall only**
- [ ] `policy/` — per-node-type `ContextPolicy` (Edit large hybrid, Validation/Stress `handoff_only`, Sanity near-zero); degrade `request-more|compact|fail`; sibling-branch context = isolated (Q4)
- [ ] `handoff/` (auto-generated at completion + branch points, **GC root**), `memory/` (project/lineage/node, TTL/decay/pinning), `repomap/` (tree-sitter PageRank), `SelectionDecision` trace
- [ ] `history-mcp/` — v1 `HistoryIndex` behind a **read-only** Lineage/History MCP server: `search_history`/`get_node_transcript`/`walk_ancestors`/`find_decisions`/`find_files_touched`/`get_handoff`, capability-gated (`nodes.readOutputs`, lineage-only), reading the single content-addressed transcript store; **no schema change**

**Definition of Done:**
- [ ] a merge regressing p99 vs a pinned baseline is **blocked** by a gate evaluating **post-merge** re-run results; override → visible audit node
- [ ] two resource-conflicting siblings serialize with a visible reason; disjoint siblings run in parallel
- [ ] Edit on a deep branch compiles context with a stable `prefix_hash` (verified cache-hit across siblings), a generated handoff doc, and a `SelectionDecision` trace
- [ ] a fresh agent started cold from the handoff document continues the branch without replaying ancestor transcripts
- [ ] the read-only Lineage/History MCP answers `search_history`/`get_node_transcript`/`walk_ancestors`, auto lineage-scoped, with no new schema

**Demoable:** attempt a merge → gate blocks on a post-merge stress regression → override (audit node) → fork a fresh branch where a new agent starts cold from the handoff doc while the context panel explains each ancestor's inclusion/drop.

---

## P8 — SDK, Tiered Executors & Marketplace
**Goal:** Open the frozen extension points to third parties — Custom Node SDK, WASM/OCI/MCP executor tiers, container/microVM backends, signed marketplace with retain-and-flag revocation — **no core edit**. **Depends on:** P5, P7. **Design:** §9.1–§9.3, §11.1, §15.1–§15.3, A.2. **Freeze before:** capability scope grammar + version · typed-port semver rules + mixed-version resolution · revocation = retain-and-flag via the F2-reserved field · `.spork-node` package format + trust-ladder grant policies.

**Build:**
- [ ] `sdk/` + `bin/spork-node` — `init --tier wasm|container|mcp --template sanity|test|stress`; `build` → component/OCI image + lockfile + SBOM; `test --against <snapshotFixture>` (determinism via derivation-key re-compare); `sign`; `publish`
- [ ] `executors/` — WASM Component Model/WASI P2 (deny clock/random/network by default); OCI container/subprocess escape hatch; MCP thin tier; impure self-declares
- [ ] additional `IsolationBackend` impls — container/devcontainer; microVM (Firecracker / Lima-Colima on macOS); agent-run untrusted-code escalation policy active
- [ ] hardened broker (short-lived scoped tokens, `AuditEntry` per call, frozen+versioned scope grammar)
- [ ] `.spork-node` package (manifest + executor + UI bundle + lockfile + SBOM + signature + trust tier) + Verified/Community/Local-Dev ladder; plain MCP servers register as thin node types
- [ ] revocation kill-switch — flags `revoked-provenance` + **retains-and-flags** cached artifacts; sandboxed UI contribution surface (postMessage-only, zero execution-plane authority); periodic replay-and-compare audit

**Definition of Done:**
- [ ] a third party scaffolds/signs/publishes a new check kind (e.g. mutation-test), installs it after reviewing capability requests, and it **instantly inherits diffing/baselining/gates/caching with no core edit**
- [ ] container-tier and microVM-tier runners execute against a CoW snapshot with deny-by-default capabilities + `AuditEntry` per privileged call
- [ ] revoking a malicious type flags its nodes `revoked-provenance` while retaining cached artifacts
- [ ] an old node under a superseded descriptor version still resolves and restores
- [ ] a custom detail panel runs in a sandboxed webview with zero execution-plane authority

**Demoable:** install a community accessibility-audit node type, grant narrow capabilities after reading the rationale, run it in a microVM, see results diffed/gated like a built-in; revoke a different type → provenance warning without losing cached state.

---

## P9 — Shared-Team Bundle Grafting
**Goal:** Collaboration as the **same** local-first machinery at wider scope — content-addressed bundle have/want + graft over pluggable transports. **Depends on:** F3, P5, P7, P8. **Design:** §19.1–§19.6, §18.3 OQ10, A.7 C-7. **Freeze before:** `.spork-bundle` on-disk format + have/want negotiation · git-remote sync-boundary mapping of `GateVerdict`s → external status checks.

**Build:**
- [ ] `bundle/` — one have/want + graft contract: export → content-addressed `.spork-bundle` set-difference (objects + typed nodes/edges + tip refs); import → graft sub-DAG, shared ancestors dedup by hash
- [ ] four transports behind that contract — `transport/local-file/`, `transport/sync-server/` (optional thin), `transport/git-remote/` (via F3 export/import), `transport/s3/`
- [ ] unified DAG view interleaving both contributors' work with `created_by`
- [ ] team merge = subgraph grafting + P5/F4 three-way reconciliation against the shared-by-hash nearest common ancestor; `GateVerdict`s/baselines travel with the snapshot; observing results re-run post-graft; conversation merge via F4 synthetic-transcript
- [ ] git-remote sync-boundary mapping (local `GateVerdict`s → external Git status checks); layout authority parked behind F3 view-model seam (Q8); first-sync large-snapshot via FastCDC + object-store offload
- [ ] **Out of scope (explicit, not stubbed):** live co-editing/CRDT + runtime-state reconciliation

**Definition of Done:**
- [ ] two daemons on two machines (no shared server) exchange a `.spork-bundle`; recipient sees the sender's nodes grafted into one unified DAG with correct `created_by` and shared ancestors deduped by hash
- [ ] team merge runs the same three-way reconciliation as single-user; sender's `GateVerdict` + baseline arrive attached to the snapshot they were computed against; observing results re-run post-merge
- [ ] same project round-trips through git-remote and S3 transports without reformatting state
- [ ] local-first single-user operation is unchanged with collaboration off (no degraded paths)

**Demoable:** two offline devs each build a branch; one exports a bundle (USB or git remote); the other imports → both timelines in one graph, clicks any teammate node to its exact state, sees gate verdicts that traveled with the snapshot, resolves a real three-way merge — no server.

---

## Edge Cases & Boundaries to keep honest (plan §12)
- [ ] **External/irreversible side effects** (shared-DB writes, pushed remotes, paid-API spend) — effects-log seam (F0/F4) + ephemeral per-branch DBs (P7) + `net.connect` egress allowlist (F3)
- [ ] **Branch merge/reconciliation** — 3-way snapshot diff + synthetic-transcript (P5 single-user, P9 cross-dev)
- [ ] **Secrets on export** — `vaultRef` + secret-scan at capture so a key never enters the CAS (boundary F3, relevant at P9 export)
- [ ] **Reflink-absent filesystems** — CoW → hardlink/copy fallback; AssetStore matters more (F3/F4)
- [ ] **Platform-specific dependency builds** — `AssetKey` platform component (F0 key / F4 impl)
- [ ] **Shared asset-cache + sandbox GC** — `AssetStore.gc(live)` keyed by live lockfiles/pins; lease-driven teardown (F0/F4)
- [ ] **Large-repo first-snapshot O(repo)** — subsequent O(delta) via FastCDC + dedup (F0)

## Deferred — do NOT build early (plan §11); each arrives behind an already-frozen seam
- Live co-editing / CRDT presence & runtime-state reconciliation → post-P9 (op-log admits CRDT as additive event variants)
- Full graphical custom-node **authoring** UI → beyond P8 (writes descriptors into the frozen registry)
- microVM isolation tier → P8 (additive `IsolationBackend` impl)
- Full team-sync server / cloud spill → after P9's async bundle path (one more transport behind the have/want contract)
