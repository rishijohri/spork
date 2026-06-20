# Spork — Implementation TODO

> Task tracker derived 1:1 from [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md). Every task traces to a plan phase (§6) or cross-cutting concern (§7). Authoritative spec is [DESIGN.md](DESIGN.md) — cite its sections in code/PRs.

## How to use this file
- The spine is **strictly linear and non-reorderable**: `F0 → F1 → F2 → F3 → F4 → P5 → P6 → P7 → P8 → P9`. A phase starts only when the previous phase's **Definition of Done is fully checked** and its **freeze-before** contracts are locked.
- **C1 (no stubs):** every phase ships *narrow-but-complete* — one real impl behind each new seam, zero placeholders. The CI no-stub gate (§8.3) forbids `todo!()`/`unimplemented!()`/`TODO`/`FIXME` in `src/**`.
- **C2 (no domino):** never change a frozen contract in place — grow it via a new impl behind the seam or a new versioned generation. See the Domino-Risk Register (plan §9, D-1…D-15) before touching anything foundational.
- **C5 (evolution-safe):** every persisted struct carries a `schema_version` + a registered migration from its first commit.
- Status: `[ ]` todo · `[~]` in progress · `[x]` done. Keep this file updated as the single source of progress.
- **At the end of EVERY phase (required):** (1) re-verify yourself — run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` from the repo root (prefix cargo with `export PATH="$HOME/.cargo/bin:$PATH"`); **do not trust an agent's self-report — they can be stale (e.g. fmt)**; for the no-stub check grep for the **macros** `grep -rnE 'todo!\(|unimplemented!\(|unreachable!\(' crates/*/src` (must be empty) — do NOT grep bare `TODO`/`FIXME`/`XXX`, which `spork-runner` legitimately contains as its detection patterns (see plan §8.3 exclusion); (2) check off the phase's Build + DoD boxes and append a one-line `✅ verified (N tests green)` note to the phase; (3) update the **Current status** line below; (4) commit the code as `Fx: <name> …` and the doc update as `docs: mark Fx done …`. A box is only `[x]` once independently re-verified green.

**Current status:** **🎉 FOUNDATION COMPLETE — F0–F4 all done.** 26 crates, **826 tests green**, clippy/fmt clean, no stub macros. F0 byte-identity · F1 event-sourcing · F2 typed-graph · F3 (headless) daemon/security/drift/restore/git · F4 execution/result/provider/context seams. Every load-bearing contract is frozen with exactly one real impl; the feature phases **P5–P9 are now purely additive behind these seams**. Plus **P5 complete** — 27 crates, **913 tests green**: six built-in node types (Edit/Validation/Stress/Sanity + Merge/Snapshot) through the public registry; Edit auto-runs Sanity; append-only results; 3-way merge; import-as-snapshot; additive-only. Committed on `feat/f0-byte-identity`. **Roadmap:** ✅F0–F4 ✅P5 → **F3-UI** (immediately after P5; code-unblocked, needs only a GUI environment — see §F3-UI) → P6 → P7 → P8 → P9. Next buildable-here step: **P6 — Multi-Provider Routing & Cost Ledger**.

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
- [x] `crates/spork-log/` — single serializing **writer actor** (the only write path); SQLite-WAL `event` table; frozen BLAKE3 hash-chain (golden-pinned); `verify_chain` tamper detection; migration-on-read
- [x] `crates/spork-projection/` — pure `fn(log) -> projection`; canonical-hashed checkpoints; `rebuild_from_log`/`load_or_rebuild`; `EventTypeCounts` reference projection
- [x] `crates/spork-migrate/` — `EventMigration` + `MigrationRegistry` keyed by `(EventType, from_version)`; chain upgrade applied on read

**Definition of Done:**
- [x] replaying the full log reproduces the projection **bit-for-bit**
- [x] delete projection DB → rebuild yields identical projection (A.6)
- [x] tampering with any stored event is detected via the hash chain (HashChainBroken at the right seq)
- [x] bumping an event `schema_version` reads old events through migration with **no stored-event rewrite**
- [x] crash-injection: object-fsync-before-log-commit → reclaimable orphan, never dangling ref (deterministic test w/ spork-cas)
- [x] append throughput **≥ 2k events/s** through the writer actor (measured ~8–18k/s on M3, release)

**Demoable:** append events, delete the projection DB, replay byte-identically, then bump a schema version and watch old events migrate on read. ✅ verified (289 tests green).

---

## F2 — Typed-Graph Contracts as Data
**Goal:** Freeze the type system *as data* before any concrete type exists. **Depends on:** F1. **Design:** §6.2, §6.3, §6.5, §7.1–§7.3, §9.1, §9.2, §14.5, A.1.

**Freeze before starting:** `NodeEnvelope` field set + column-vs-JSON split · `NodeTypeDescriptor` schema + `ownsSnapshot`-at-registration rule + built-ins-use-the-same-registry · typed-edge set + acyclicity-on-insert + refs-as-GC-roots · `payloadSchemaVersion` lazy-upgrade + mixed-descriptor-version retention · reserved `revoked-provenance` field (Q9 direction).

**Build:**
- [x] `crates/spork-graph/` — `NodeEnvelope` (hot fields `status`/`is_stale`/`snapshot_hash` as **indexed columns, not JSON**); graph events; SQLite graph projection over the F1 log; `GraphService` validate-then-append command layer; `lineage_hash`; lazy payload upgrade; one dogfooded built-in
- [x] `crates/spork-registry/` — `NodeTypeRegistry.{register,resolve,list}`; rejects `ownsSnapshot`/`contentRef` mismatch + duplicate version; mixed-version retention; reserved `revoked_provenance`; built-ins register through the **public** path
- [x] `crates/spork-edges/` — typed `EdgeType` set, `would_create_cycle` acyclicity, `RefKind` (refs as GC roots)
- [x] `crates/spork-status/` — `effective_status` fold (one token per `(Lifecycle, is_stale)`, incl. `Cancelled`)

**Definition of Done:**
- [x] descriptor claiming `ownsSnapshot` with no `SnapshotRef` out-port → **refused at registration** (and `create_node` owns_snapshot-without-hash refused)
- [x] cycle-creating edge → rejected against the projection; graph stays a DAG (proptest w/ Kahn witness)
- [x] node under `typeVersion` 1.0.0 still resolves/restores after 2.0.0 registers (descriptors retained, not replaced)
- [x] older-payload node reads back via lazy upgrade (spork-migrate), raw stored row unchanged
- [x] `effective_status` returns exactly one token for every `(Lifecycle, is_stale)` pair incl. `Cancelled`
- [x] drop-and-rebuild graph projection from the log → identical canonical digest

**Demoable:** register a descriptor exactly as a third party would, create nodes/edges, watch `lineage_hash` populate, see a cycle edge rejected and a malformed descriptor refused. ✅ verified (396 tests green). **Review fixed 2 frozen-contract defects: descriptor-driven `family` (not a kind heuristic), and `MERGE_PARENT` symmetric parent/child (so staleness invalidation reaches merge nodes).**

---

## F3 — Daemon Seam, Security Boundary & Interactive Core (HEADLESS CORE)
**Goal:** Freeze the daemon/renderer split + security boundary and ship the first interactive slice **driven by a headless client over the real IPC** — **retire the untracked-mutation gap and the chat-only-restore trap here**. **Depends on:** F2. **Design:** §5.5, §6.4, §10.1–§10.4, §14.1, §15.1, §15.2, §15.4, A.1, A.3, A.6.

> **Scope note:** F3 is split. The **headless core** below is built + verified now (it is the daemon, IPC, security, drift, restore, and git boundary — all testable without a GUI). The **Tauri/React DAG canvas is split out as `F3-UI`** (next section) because a desktop/webview UI cannot be built or visually verified in a headless CLI sandbox. The IPC/event/capability/view-model contracts are frozen here, so `F3-UI` is purely additive whenever it lands (no rework).

**Freeze before starting:** IPC command/event envelope + `opId`-returns-then-state-via-events · durable-on-ordered-stream vs ephemeral-on-side-channel split · capability vocabulary + versioned scope grammar + `AuditEntry`-per-call · `vaultRef` indirection (secrets resolved only in daemon, never hashed) · `AttributionRecord` schema + A.3 precedence · **the view-model boundary the future renderer binds to (frozen now so F3-UI is additive)**.

**Build (headless core):**
- [x] `crates/spork-ipc/` — typed command/event contract; mutations return `opId`, state arrives only via the ordered event stream; forward-tolerant `OpLogEvent`s
- [x] `crates/spork-stream/` — dual delivery: unbounded ordered event rail + bounded node-id-keyed ephemeral side-channels (drop-on-full, never stalls ordered)
- [x] `crates/spork-broker/` — deny-by-default `CapabilityBroker` + versioned `Scope` grammar; `AuditEntry` on every allow AND deny
- [x] `crates/spork-vault/` — `VaultRef` (OS-CSPRNG, opaque) + non-`Serialize` zeroizing `Secret` + `FileVault` (0600); secrets never enter the CAS
- [x] `crates/spork-drift/` — fused interceptor + FS watcher (notify) + reconciliation rescan + buffer bridge; A.3 precedence `AttributionRecord`; secret-scan-at-capture
- [x] `crates/spork-restore/` — atomic dual-restore guard (one lock, verify-then-mutate, fail-closed); metadata-only `branch_fork`; shaped effects-log slot
- [x] `crates/spork-git/` — non-invasive `GitContext` (git2 vendored); `import_git_state`/`export_to_git`; HEAD/index/worktree byte-unchanged
- [x] `crates/spork-daemon/` — wires all the above behind the `CommandHandler` dispatch; frozen `graph_view`/`subscribe_events` view-model boundary; headless client drives the full flow

**Definition of Done (headless core):**
- [x] a **headless client** drives the IPC; mutations return `opId`, state arrives as ordered events (renderer parity is an F3-UI item)
- [x] ephemeral token/stdout volume never stalls ordered event delivery
- [x] out-of-band fs `rm`/`mv`/add/modify + an unsaved-buffer case each → correctly-attributed node within the debounce window
- [x] planted fake API key caught at capture, **never enters the CAS**
- [x] side effect without a capability → denied with an `AuditEntry`
- [x] restoring an old node restores code + bound conversation atomically, **fails closed** on injected divergence (nothing changes), forward history survives as a sibling
- [x] node restore **p95 < 500 ms** (measured 28 ms release / 35 ms debug on M3)

**Demoable (headless):** via a headless client — `rm` a file via raw bash → attributed drift node; fetch a past node's exact diff; restore (code + conversation), forward history survives as a branch; export to a clean Git commit while `.git` stays byte-unchanged. ✅ verified (600 tests green). **Review fixed a restore-atomicity race + a Capability `#[non_exhaustive]`/doc contradiction; I additionally hardened `VaultRef` to the OS CSPRNG and narrowed `blob.read` authorization to the concrete path.**

---

## F3-UI — Tauri/React DAG Canvas (scheduled: immediately AFTER P5)
**Status:** **CODE-UNBLOCKED as of F4** (no hard P5 dependency — it is schema-driven off `NodeTypeDescriptor` and binds only to the F3-frozen IPC + view-model contracts, both frozen; purely additive, no rework). The ONLY remaining prerequisite is an **environment**, which this headless CLI sandbox cannot provide. **Design:** §14.1–§14.5, §15.1.

**SCHEDULED POSITION (the concrete answer):** the roadmap is `F0→F1→F2→F3-core→F4 (done) → P5 → **F3-UI** → P6 → P7 → P8 → P9`. F3-UI is the phase **immediately after P5**, so the first canvas renders the real Edit/Validation/Stress/Sanity/Merge node types. P6–P9 are backend and do NOT depend on F3-UI (they can proceed whether or not the UI has been built yet).

**One hard prerequisite — a GUI-capable environment:** a display + the Tauri/Node toolchain + a human able to do visual/interaction verification. It CANNOT be built or verified in the current headless CLI sandbox (no display/webview), so it is not attempted here. **Pull-earlier rule:** because it has no hard P5 dependency, it may be pulled in *before* P5 — even right now — the moment a GUI environment is available; in the default headless flow P5 is built first (since that IS buildable here) and F3-UI follows immediately.

**Build (when picked up):**
- [ ] `app/` (Tauri shell, Rust core) — daemon source-of-truth → denormalized virtualized view-model → React Flow; ELK layout off-thread (Web Worker); lazy CAS diff in Monaco
- [ ] five-region layout (top bar + model selector + toolbar; left navigator+legend; center DAG canvas; right Node-Details: chat+diff+results; bottom run rail)
- [ ] op-log event reducer + optimistic UI w/ `opId` reconciliation; ephemeral side-channels for tokens/stdout
- [ ] schema-driven node cards/details/legend off the `NodeTypeDescriptor`; sandboxed webview for custom UI contributions

**Definition of Done (when picked up):**
- [ ] renderer drives the **same** IPC as the headless client (parity)
- [ ] canvas **≥ 55 fps @ 1k nodes**, click→diff **p95 < 150 ms**
- [ ] click a node → exact state lazily from CAS; restore/branch from the canvas; layout positions stable as the graph grows

---

## F4 — Execution, Result, Provider & Context Seams (one impl each)
**Goal:** Freeze every remaining load-bearing contract a feature plugs into, each with **exactly one** working impl. **Depends on:** F3. **Design:** §5.3, §5.4, §8.1, §8.2, §10.3, §11.1–§11.4, §12.1–§12.3, §13.2, §6.5, A.4.

**Freeze before starting:** `IsolationBackend` + `ResourceProfile` + lease/reaper (impl: worktree-on-CoW) · `Scheduler` admission trait (impl: serial admission) · `Runner` SPI + versioned `ResultEnvelope` + metric-id registry · `inputDigest`/`derivationKey` formula + key-version-per-artifact + declaredInputs honesty + impure opt-out · `ProviderAdapter` port + canonical transcript + `OpaqueProviderBlock` · `ContextLayer` volatility ranks + `prefix_hash` (impl: single-turn `ContextCompiler` + single-node `HandoffGenerator`) · `EnvManifest` schema + canonical serialization (hashed into identity) · Merge `conflictResolution` + synthetic-transcript schemas · **v1 `AssetStore`** plugged into `IsolationBackend.provision`. **Pin as data/policy:** Q3 isolation tier, Q5 restore scope (code+conversation + shaped effects-log seam), Q6 inherit-parent model, Q9 retain-and-flag, handoff-as-GC-root.

**Build:** (crates `spork-exec`, `spork-runner`, `spork-provider`, `spork-context`, `spork-merge`)
- [x] `IsolationBackend` (v1: `WorktreeCowBackend`; reflink/clonefile, hardlink/copy fallback) + `ResourceProfile` + durable lease ledger + crash-safe reaper
- [x] `Scheduler.admit` v1 — serial admission (one-at-a-time, serialize-with-reason)
- [x] `Runner` SPI (`describe`/`prepare`/`run`/`normalize`/`collect_artifacts`) + versioned `ResultEnvelope` (units/metrics/violations/artifactManifest) + metric-id registry + v1 `SanityRunner`
- [x] content-addressed `input_digest`/`DerivationKey` cache (incl. **change scope** — review fix; per-artifact key-version/generation; impure never caches)
- [x] `ProviderAdapter` port + v1 `AnthropicAdapter` (canonical↔wire **mapping only**, offline) + `SingleProviderRouter` (`PrivacyClass`-enforcing, `next_fallback`) + `ProviderProjection`
- [x] `CanonicalTranscript` (versioned); content-addressed + bindable to a snapshot for dual-restore (cross-seam test in `spork-restore`)
- [x] `ContextCompiler` v1 (single-turn, stable→volatile by frozen `ContextLayer` ranks, `prefix_hash`-keyed) + `HandoffGenerator` v1 (single-node) + `SelectionDecision` trace
- [x] `EnvManifest` (toolchains/lockfile hashes/base-image digest), canonical-serialized + hashed into node identity
- [x] v1 `AssetStore` wired into `provision` (opaque class from F0 `LocalCasAssetStore`; **ecosystem resolvers npm/pip/cargo are additive — P5/P8**) + `spork-merge` (`ConflictResolution` + `SyntheticTranscript`)

**Definition of Done:**
- [x] Sanity `CheckSpec` auto-runs change-scoped after an edit, stores an append-only `ResultEnvelope`
- [x] identical `(spec, inputTreeHash, runner_version, scope)` re-run is a **cache hit**; an impure runner never caches
- [x] tests-shaped, perf-shaped, and lint-shaped results all round-trip through the **same** `ResultEnvelope` (no core code knows the runner type)
- [x] a chat turn round-trips through `AnthropicAdapter` via the canonical transcript and restores with its code under the dual-restore guard
- [x] two nodes with the same `EnvManifest` hash are environment-identical
- [x] dead-owner lease reclaimed by the reaper on restart
- [x] `ContextCompiler` `prefix_hash` stable across siblings + unaffected by volatile tail; regenerable handoff; pinned OQ resolutions are data/policy

**Demoable:** register a Sanity `CheckSpec`, run → normalized `ResultEnvelope`, re-run → content-addressed cache hit; send a chat turn through Anthropic stored as a provider-agnostic transcript that restores with its code. ✅ verified (826 tests green). **Review fixed a cache-soundness defect (change-scope omitted from `input_digest` → a scoped run could stale-hit a full scan); now folded in with regression tests.**

---

## P5 — Four Built-In Node Types
**Goal:** Ship the four product built-ins (+ Merge, Snapshot) through the same F2 registry + F4 Runner SPI. **Depends on:** F3, F4. **Design:** §6.2, §6.5, §7.1, §8.1, §8.2, A.4. **Freeze before:** the four built-in payload schemas (versioned).

**Build:** (new crate `spork-nodes` + additive variants in `spork-ipc` + additive wiring in `spork-daemon`)
- [x] Edit (`spork-nodes::edit`) — Mutating, owns_snapshot; binds **both** `contentRef` and `conversation_ref`; `diff_summary`/`files_changed`/`tool_calls`/`context_sources`
- [x] Validation + Stress (`spork-nodes::validation`/`stress`) — Observing runners behind the F4 SPI (JUnit-XML → `ResultEnvelope.units`; p50/p95/p99/throughput/peak-mem metrics)
- [x] Sanity (`spork-nodes::sanity`) — auto-run change-scoped, debounced, hermetic (reuses the F4 SanityRunner)
- [x] Merge (`spork-nodes::merge`) — 3-way reconciliation (nearest common ancestor) → clean materializable Merge node; conflicts return a conflict set (no half-node); observing children re-run post-merge. *(Visual Monaco conflict UI is F3-UI.)*
- [x] Snapshot (`spork-nodes::snapshot`) — `origin = auto_drift | manual | import` (Import is a Snapshot, not a separate kind — A.7 C-2)
- [~] data-driven node card / details / legend off the descriptor — **data is defined** (`ui_contributions` on the descriptor); the actual rendering is **F3-UI**

**Definition of Done:**
- [x] all six (Edit/Validation/Stress/Sanity + Merge/Snapshot) register through the public registry exactly as a third party would (zero special-casing; `register_builtins` → public `register_descriptor`)
- [x] Edit auto-triggers Sanity with cache hits on unchanged subtrees (CHECK_SCHEDULED → RESULT_RECORDED; cache hit via F4 input_digest)
- [x] Validation/Stress attach append-only results without mutating the parent (parent snapshot_hash unchanged; observing children via VALIDATES/STRESSES edges)
- [x] branch → alternate Edit → merges via 3-way into a clean, materializable Merge node whose observing results re-run post-merge; conflicting merge returns a conflict set, no half-node
- [x] Import ingests external state as an `origin=import` Snapshot node

**Demoable:** run an Edit, watch Sanity auto-run + Validation/Stress attach results; branch, merge through 3-way into a Merge node whose checks re-run against the merged tree. ✅ verified (913 tests green); additive-only — no frozen contract changed.

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
