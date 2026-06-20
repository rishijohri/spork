// Hand-written TypeScript mirrors of the FROZEN F3 IPC + view-model Rust types
// (DESIGN.md §14.3–§14.5, A.1). These shapes are verified against the Rust serde
// attributes in:
//   - crates/spork-ipc/src/{command,result,event,ephemeral}.rs
//   - crates/spork-daemon/src/view.rs
//   - crates/spork-{status,edges,registry}/src/lib.rs (enum tags)
//
// Wire conventions (do not drift):
//   - Command  : internally tagged on "command", SCREAMING_SNAKE_CASE tags,
//                camelCase fields.
//   - CommandResult : internally tagged on "result", SCREAMING_SNAKE_CASE tags,
//                camelCase fields.
//   - OpLogEvent : internally tagged on "type", SCREAMING_SNAKE_CASE tags,
//                camelCase fields ("ref" stays "ref", not "refName").
//   - EphemeralFrame : camelCase, channel is SCREAMING_SNAKE_CASE string.
//   - GraphView / NodeView / EdgeView / RefView : camelCase fields.
//   - Hash serializes as a string; Ulid serializes as a string.

/** A BLAKE3 content hash, as the renderer-portable hex/tagged string. */
export type Hash = string;
/** A ULID, time-sortable, as a 26-char Crockford string. */
export type Ulid = string;

// --- Enum tags (verified against the Rust serde rename_all attributes) --------

/** spork-registry Family: serde `snake_case`. */
export type Family = "mutating" | "observing" | "context";

/** spork-status Lifecycle: serde `snake_case`. */
export type Lifecycle =
  | "pending"
  | "running"
  | "passed"
  | "failed"
  | "blocked"
  | "cancelled";

/** spork-edges EdgeType: serde `SCREAMING_SNAKE_CASE`. */
export type EdgeType =
  | "PARENT_CHILD"
  | "BRANCH"
  | "DERIVED_FROM"
  | "VALIDATES"
  | "CHECKS"
  | "STRESSES"
  | "MERGE_PARENT";

/** spork-edges RefKind: serde default (PascalCase) — `"Head" | "Branch" | "Tag"`. */
export type RefKind = "Head" | "Branch" | "Tag";

// --- View-model (crates/spork-daemon/src/view.rs) -----------------------------

/** The renderer-facing projection of one node (a card on the canvas). */
export interface NodeView {
  id: Ulid;
  kind: string;
  family: Family;
  status: Lifecycle;
  isStale: boolean;
  ownsSnapshot: boolean;
  /** Present iff `ownsSnapshot`. */
  snapshotHash: Hash | null;
  branchId: string;
  parentIds: Ulid[];
  model: string | null;
}

/** The renderer-facing projection of one typed edge. */
export interface EdgeView {
  from: Ulid;
  to: Ulid;
  edgeType: EdgeType;
}

/** The renderer-facing projection of one ref (HEAD / branch / tag). */
export interface RefView {
  name: string;
  kind: RefKind;
  target: Ulid;
}

/** A denormalized read snapshot of the whole work-DAG (the view-model boundary). */
export interface GraphView {
  schemaVersion: number;
  nodes: NodeView[];
  edges: EdgeView[];
  refs: RefView[];
}

// --- Commands (crates/spork-ipc/src/command.rs) -------------------------------

/** A typed request from the renderer to the daemon (tag field: "command"). */
export type Command =
  | {
      command: "NODE_CREATE";
      kind: string;
      typeVersion: string;
      parentIds: Ulid[];
      branchId: string;
      payload: unknown;
      ownsSnapshot: boolean;
      snapshotHash: Hash | null;
    }
  | { command: "NODE_RESTORE"; nodeId: Ulid }
  | { command: "BRANCH_FORK"; fromNodeId: Ulid; name: string }
  | { command: "REF_CREATE"; name: string; kind: RefKind; to: Ulid }
  | { command: "REF_MOVE"; name: string; to: Ulid }
  | { command: "OP_UNDO"; opId: Ulid | null }
  | { command: "OP_REDO"; opId: Ulid | null }
  | { command: "GC_RUN"; dryRun: boolean }
  | { command: "NODE_DIFF"; nodeId: Ulid; against: Ulid | null }
  | { command: "BLOB_READ"; treeHash: Hash; path: string }
  | { command: "NODE_RUN_CHECK"; targetNodeId: Ulid; spec: unknown }
  | {
      command: "BRANCH_MERGE";
      intoRef: string;
      fromNodeId: Ulid;
      resolution: unknown | null;
    }
  // The F3-UI git actions (DESIGN.md §10.4). Action-shaped, NOT graph mutations:
  // they reply inline with a `Git` CommandResult and emit no OpLogEvent. Field
  // casing matches the Rust serde `rename_all_fields = "camelCase"` (nodeId,
  // branch / nodeId, remote); the `Option<String>` fields are `string | null`.
  | { command: "GIT_EXPORT"; nodeId: Ulid; branch: string | null }
  | { command: "GIT_PUSH"; nodeId: Ulid; remote: string | null };

/** The command tag literal type, for exhaustive switching. */
export type CommandTag = Command["command"];

// --- Command results (crates/spork-ipc/src/result.rs) -------------------------

/** The typed reply to a Command (tag field: "result"). */
export type CommandResult =
  | { result: "MUTATION"; opId: Ulid; ids: Record<string, unknown> }
  | { result: "DIFF"; changedPaths: string[] }
  | { result: "BLOB"; bytes: number[] }
  | { result: "GC"; reclaimable: string[]; bytes: number }
  // The reply to GIT_EXPORT / GIT_PUSH (DESIGN.md §10.4). Returned inline (these
  // are action-shaped, not mutations): the branch the snapshot was projected to,
  // the commit SHA, and whether it was pushed. Field casing matches the Rust
  // serde `rename_all_fields = "camelCase"` (commitSha).
  | { result: "GIT"; branch: string; commitSha: string; pushed: boolean };

// --- Op-log events (crates/spork-ipc/src/event.rs) ----------------------------
//
// NOTE: the moved/created/forked ref-name field is the literal wire key `ref`,
// NOT `refName` — the Rust struct renames it via `#[serde(rename = "ref")]`.

/** A durable, ordered op-log event the renderer reduces over (tag: "type"). */
export type OpLogEvent =
  | { type: "NODE_CREATED"; seq: number; nodeId: Ulid; schemaVersion: number }
  | { type: "EDGE_ADDED"; seq: number; from: Ulid; to: Ulid; edge: EdgeType }
  | { type: "REF_MOVED"; seq: number; ref: string; to: Ulid }
  | { type: "REF_CREATED"; seq: number; ref: string }
  | { type: "BRANCH_FORKED"; seq: number; ref: string }
  | { type: "RESTORE_PERFORMED"; seq: number; nodeId: Ulid }
  | { type: "OP_UNDONE"; seq: number }
  | { type: "OP_REDONE"; seq: number }
  | { type: "GC_PERFORMED"; seq: number }
  | { type: "RESULT_RECORDED"; seq: number; runId: Ulid; nodeId: Ulid }
  | { type: "MERGE_PERFORMED"; seq: number; nodeId: Ulid }
  | { type: "CHECK_SCHEDULED"; seq: number; runId: Ulid };

/** The op-log event tag literal type. */
export type OpLogEventTag = OpLogEvent["type"];

// --- Ephemeral frames (crates/spork-ipc/src/ephemeral.rs) ---------------------

/** Which ephemeral side-channel a frame belongs to: serde SCREAMING_SNAKE_CASE. */
export type EphemeralChannel = "CHAT_TOKENS" | "RUN_STDOUT";

/** One frame of high-frequency, transient data for a node (no seq). */
export interface EphemeralFrame {
  nodeId: Ulid;
  channel: EphemeralChannel;
  data: string;
}

// --- Tauri event payload envelopes -------------------------------------------
//
// The backend forwards daemon events to the window with these event names; the
// payload is the event/frame above. Matches src-tauri/src/lib.rs emit() calls.

/** The Tauri window event name carrying an `OpLogEvent`. */
export const OPLOG_EVENT = "oplog-event" as const;
/** The Tauri window event name carrying an `EphemeralFrame`. */
export const EPHEMERAL_EVENT = "ephemeral" as const;

/** A Tauri event payload as delivered to a `listen` handler. */
export interface TauriEvent<T> {
  event: string;
  id: number;
  payload: T;
}
