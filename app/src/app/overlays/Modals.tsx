// Modal host (UI_UX_DESIGN.md §5.10).
//
// Reads the single open modal from the store and renders it. Every modal maps to
// a BUILT command and dispatches through the shared action runner (useActions),
// so feedback (activity line, optimistic op, capability-denial routing) is
// uniform. The capability modal surfaces a denial honestly; inline grant is
// forward-map (P8), so it points the user at the daemon config instead.

import { useState, type JSX } from "react";
import { useUiStore } from "../../state/store";
import { useActions } from "../useActions";
import { Modal } from "../../ui/Modal";
import { Button } from "../../ui/Button";
import { Icon } from "../../ui/icons";
import { shortId, humanizeModel, formatMicroUsd } from "../../ui/format";
import { availableModels } from "../TopBar";
import type { AgentRunIntent, CostView, NodeView } from "../../ipc/types";

/** Renders whichever modal the store says is open. */
export function Modals(): JSX.Element | null {
  const modal = useUiStore((s) => s.modal);
  const view = useUiStore((s) => s.view);
  if (!modal) return null;

  const node = (id: string): NodeView | null =>
    view.nodes.find((n) => n.id === id) ?? null;

  switch (modal.kind) {
    case "merge":
      return <MergeModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
    case "restore":
      return <RestoreModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
    case "commit":
      return <CommitModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
    case "push":
      return <PushModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
    case "gc":
      return <GcModal />;
    case "askAgent":
      return (
        <AskAgentModal
          node={node(modal.nodeId)}
          nodeId={modal.nodeId}
          initialIntent={modal.intent}
        />
      );
    case "capability":
      return <CapabilityModal capability={modal.capability} action={modal.action} />;
    default:
      return null;
  }
}

/** A safe number/string read from a mutation reply's `ids` bag (unknown-typed). */
function numId(ids: Record<string, unknown>, key: string): number {
  const v = ids[key];
  return typeof v === "number" ? v : 0;
}
function strId(ids: Record<string, unknown>, key: string, fallback = ""): string {
  const v = ids[key];
  return typeof v === "string" ? v : fallback;
}

/** Ask the agent — NODE_AGENT_RUN (read-only; attaches a context node). */
function AskAgentModal({
  node,
  nodeId,
  initialIntent,
}: {
  node: NodeView | null;
  nodeId: string;
  /** Pre-selected agentic mode when opened from a "+ New node ▾" item (R2). */
  initialIntent?: AgentRunIntent | undefined;
}): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const defaultModel = useUiStore((s) => s.defaultModel);
  const agentProvider = useUiStore((s) => s.agentProvider);
  const localModels = useUiStore((s) => s.localModels);
  const attachAgentNode = useUiStore((s) => s.attachAgentNode);
  const attachEditNode = useUiStore((s) => s.attachEditNode);
  const selectNode = useUiStore((s) => s.selectNode);
  const models = availableModels(agentProvider, localModels);
  const [prompt, setPrompt] = useState("");
  const [model, setModel] = useState(defaultModel || models[0] || "");
  const [privacy, setPrivacy] = useState("any");
  const [intent, setIntent] = useState<AgentRunIntent>(initialIntent ?? "ask");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{
    model: string;
    cost: CostView;
    edit?: { branchId: string; forked: boolean; sanity: string | null };
  } | null>(null);

  const isChange = intent === "change";

  async function ask(): Promise<void> {
    if (!prompt.trim()) return;
    setBusy(true);

    // The code-changing intent goes to NODE_AGENT_EDIT: the daemon edits a CoW
    // copy and the Edit node arrives over the op-log (no manual attach).
    if (isChange) {
      const res = await run(
        {
          command: "NODE_AGENT_EDIT",
          targetNodeId: nodeId,
          prompt: prompt.trim(),
          modelKey: model,
          privacy,
        },
        { nodeId, label: "Make change" },
      );
      setBusy(false);
      if (res && res.result === "MUTATION") {
        const ids = res.ids;
        setResult({
          model: strId(ids, "model", model),
          cost: { inputTokens: 0, outputTokens: 0, microUsd: numId(ids, "costMicroUsd") },
          edit: {
            branchId: strId(ids, "branchId", node?.branchId ?? "main"),
            forked: ids["forked"] === true,
            sanity: typeof ids["sanity"] === "string" ? (ids["sanity"] as string) : null,
          },
        });
        const editId = strId(ids, "editNodeId");
        if (editId) {
          const editNode: NodeView = {
            id: editId,
            kind: "codebase-edit",
            family: "mutating",
            status: "passed",
            isStale: false,
            ownsSnapshot: true,
            snapshotHash: "b3:edit",
            branchId: strId(ids, "branchId", node?.branchId ?? "main"),
            parentIds: [nodeId],
            model: strId(ids, "model", model),
            cost: { inputTokens: 0, outputTokens: 0, microUsd: numId(ids, "costMicroUsd") },
            gate: null,
            // R2: an optimistic Edit node — the authoritative line label/state
            // arrive on the next graph_view refetch. A fork records its origin.
            presentationStatus: null,
            lineLabel: strId(ids, "branchId", node?.branchId ?? "main"),
            forkedFrom: ids["forked"] === true ? nodeId : null,
          };
          attachEditNode(editNode, nodeId);
          selectNode(editId);
        }
      }
      return;
    }

    const res = await run(
      {
        command: "NODE_AGENT_RUN",
        targetNodeId: nodeId,
        prompt: prompt.trim(),
        modelKey: model,
        privacy,
        intent,
      },
      { nodeId, label: "Ask agent" },
    );
    setBusy(false);
    if (res && res.result === "MUTATION") {
      const ids = res.ids;
      const cost: CostView = {
        inputTokens: numId(ids, "inputTokens"),
        outputTokens: numId(ids, "outputTokens"),
        microUsd: numId(ids, "costMicroUsd"),
      };
      const attached: NodeView = {
        id: strId(ids, "nodeId"),
        kind: "agent-context",
        family: "context",
        status: "passed",
        isStale: false,
        ownsSnapshot: false,
        snapshotHash: null,
        branchId: node?.branchId ?? "main",
        parentIds: [],
        model: strId(ids, "model", model),
        cost,
        gate: null,
        // R2: a context node attaches on the current line — never forks.
        presentationStatus: null,
        lineLabel: node?.branchId ?? "main",
        forkedFrom: null,
      };
      attachAgentNode(attached, nodeId);
      setResult({ model: attached.model ?? model, cost });
    }
    // A capability denial is routed to the capability modal by `run`; this modal
    // stays open so the prompt is not lost.
  }

  return (
    <Modal
      title={isChange ? "Ask the agent to make a change" : "Ask the agent"}
      icon="messages-square"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            {result ? "Done" : "Cancel"}
          </Button>
          <Button variant="primary" busy={busy} disabled={!prompt.trim()} onClick={() => void ask()}>
            {isChange ? "Make change" : "Ask"}
          </Button>
        </>
      }
    >
      {isChange ? (
        <p className="spork-modal-note">
          A <strong>code-changing</strong> run against <code>{shortId(nodeId)}</code>:
          the agent edits a <em>copy</em> of your code in an isolated worktree and
          creates a new <em>Edit</em> node (diff + transcript + auto-Sanity). Your
          real working directory is never touched; from a non-tip node it auto-forks
          a branch (DESIGN §6.6, §9.2).
        </p>
      ) : (
        <p className="spork-modal-note">
          A <strong>read-only</strong> run against <code>{shortId(nodeId)}</code>: the
          answer attaches as an <em>Agent</em> context node by a dotted edge — your
          code is untouched and no branch is forked (DESIGN §6.6).
        </p>
      )}
      <div className="spork-field">
        <label htmlFor="agent-prompt">Prompt</label>
        <textarea
          id="agent-prompt"
          rows={3}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="e.g. What does this module do?"
          autoFocus
        />
      </div>
      <div className="spork-field">
        <label htmlFor="agent-model">Model</label>
        {models.length === 0 ? (
          <span className="spork-muted" style={{ fontSize: 12 }}>
            No model configured — set up a provider in Settings.
          </span>
        ) : (
          <select id="agent-model" value={model} onChange={(e) => setModel(e.target.value)}>
            {models.map((m) => (
              <option key={m} value={m}>
                {humanizeModel(m)}
              </option>
            ))}
          </select>
        )}
      </div>
      <div className="spork-field">
        <label htmlFor="agent-intent">Intent</label>
        <select
          id="agent-intent"
          value={intent}
          onChange={(e) => setIntent(e.target.value as AgentRunIntent)}
        >
          <option value="ask">Ask</option>
          <option value="plan">Plan</option>
          <option value="analysis">Analysis</option>
          <option value="change">Make a change (edits code)</option>
        </select>
      </div>
      <label className="spork-row-between" style={{ cursor: "pointer" }}>
        <span>
          Local-only (refuse cloud) <span className="spork-faint">— privacy</span>
        </span>
        <input
          type="checkbox"
          checked={privacy === "local_only"}
          onChange={(e) => setPrivacy(e.target.checked ? "local_only" : "any")}
        />
      </label>
      {result && (
        <div
          className="spork-warn-box"
          style={{ background: "var(--bg-raised)", borderColor: "var(--border-strong)", color: "var(--fg)" }}
        >
          <Icon name={result.edit ? "git-branch" : "messages-square"} size={15} />
          {result.edit ? (
            <span>
              Edit node created on <strong>{result.edit.branchId}</strong>
              {result.edit.forked ? " (auto-forked)" : ""} ·{" "}
              {humanizeModel(result.model)} · {formatMicroUsd(result.cost.microUsd)}
              {result.edit.sanity ? ` · Sanity: ${result.edit.sanity}` : ""}.
            </span>
          ) : (
            <span>
              Answered by <strong>{humanizeModel(result.model)}</strong> ·{" "}
              {result.cost.inputTokens}↑/{result.cost.outputTokens}↓ tokens ·{" "}
              {formatMicroUsd(result.cost.microUsd)}.
            </span>
          )}
        </div>
      )}
    </Modal>
  );
}

function useClose(): () => void {
  return useUiStore((s) => s.closeModal);
}

/** A friendly line label for a destination branch ref (its target's line). */
function lineLabelForRef(view: NodeView[] | undefined, target: string, fallback: string): string {
  const n = view?.find((x) => x.id === target);
  return n?.lineLabel ?? n?.branchId ?? fallback;
}

/**
 * Merge into another **line** — BRANCH_MERGE (3-way), optionally through a P7
 * quality gate (REALIGNMENT_PLAN §5a). The destination is a line (presented by
 * its friendly label); branching is automatic, so there is no manual "new line".
 */
function MergeModal({ node, nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const view = useUiStore((s) => s.view);
  const { run } = useActions();
  // Destination lines are the branch refs, excluding the source node's own line.
  const destinations = view.refs.filter(
    (r) => r.kind === "Branch" && (!node || view.nodes.find((n) => n.id === r.target)?.branchId !== node.branchId),
  );
  const [into, setInto] = useState(destinations[0]?.name ?? "main");
  const [gated, setGated] = useState(false);
  const [busy, setBusy] = useState(false);

  async function merge(): Promise<void> {
    setBusy(true);
    // The P7 gate: require all post-merge sanity checks to pass before promoting
    // the branch ref (DESIGN §8.3). A blocking gate; an override is a separate
    // explicit step (the daemon's overrideReason).
    const gate = {
      schemaVersion: 1,
      id: "merge-sanity",
      transition: "merge",
      predicate: { predicate: "all_passed", args: { kind: "sanity" } },
      severity: "block",
      onFlaky: "block",
    };
    const command = gated
      ? ({
          command: "BRANCH_MERGE_GATED",
          intoRef: into,
          fromNodeId: nodeId,
          resolution: null,
          gate,
          baseline: null,
          overrideReason: null,
        } as const)
      : ({
          command: "BRANCH_MERGE",
          intoRef: into,
          fromNodeId: nodeId,
          resolution: null,
        } as const);
    const res = await run(command, { nodeId, label: gated ? "Gated merge" : "Merge" });
    setBusy(false);
    if (res) close();
  }

  return (
    <Modal
      title="Merge into line"
      icon="git-merge"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="primary" busy={busy} onClick={() => void merge()}>
            {gated ? "Merge with gate" : "Merge"}
          </Button>
        </>
      }
    >
      <p className="spork-modal-note">
        3-way merge from the line of <code>{node?.lineLabel ?? node?.branchId ?? shortId(nodeId)}</code>{" "}
        into the destination line, using the nearest common ancestor. A clean merge
        creates a Merge node; conflicts surface for resolution with no half-node.
      </p>
      <div className="spork-field">
        <label htmlFor="merge-into">Destination line</label>
        <select id="merge-into" value={into} onChange={(e) => setInto(e.target.value)}>
          {destinations.length === 0 && <option value="main">main</option>}
          {destinations.map((b) => (
            <option key={b.name} value={b.name}>
              {lineLabelForRef(view.nodes, b.target, b.name)}
            </option>
          ))}
        </select>
      </div>
      <label className="spork-checkbox">
        <input
          type="checkbox"
          checked={gated}
          onChange={(e) => setGated(e.target.checked)}
        />
        Run a quality gate (block promotion if post-merge sanity fails)
      </label>
    </Modal>
  );
}

/** Restore — NODE_RESTORE (non-destructive; effects warning). */
function RestoreModal({ node, nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const [busy, setBusy] = useState(false);

  async function restore(): Promise<void> {
    setBusy(true);
    const res = await run({ command: "NODE_RESTORE", nodeId }, { nodeId, label: "Restore" });
    setBusy(false);
    if (res) close();
  }

  return (
    <Modal
      title="Restore this node?"
      icon="corner-up-left"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="danger" busy={busy} onClick={() => void restore()}>Restore</Button>
        </>
      }
    >
      <p className="spork-modal-note">
        Brings the working tree + conversation back to{" "}
        <code>{shortId(nodeId)}</code>{node ? ` (${node.kind})` : ""}. This is an{" "}
        <strong>event, not an overwrite</strong> — your current line survives as a branch.
      </p>
      <div className="spork-warn-box">
        <Icon name="alert-triangle" size={15} />
        <span>
          External side-effects (DB writes, pushed commits, paid-API spend) are
          <strong> not</strong> undone.
        </span>
      </div>
    </Modal>
  );
}

/** Commit to Git — GIT_EXPORT. */
function CommitModal({ nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const [branch, setBranch] = useState(`spork/${nodeId.slice(-7)}`);
  const [busy, setBusy] = useState(false);

  async function commit(): Promise<void> {
    setBusy(true);
    const res = await run(
      { command: "GIT_EXPORT", nodeId, branch: branch.trim() || null },
      { nodeId, label: "Commit to Git" },
    );
    setBusy(false);
    if (res) close();
  }

  return (
    <Modal
      title="Commit to Git"
      icon="git-commit"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="primary" busy={busy} onClick={() => void commit()}>Commit</Button>
        </>
      }
    >
      <p className="spork-modal-note">
        Project this node's snapshot into a real Git commit on a new branch. Your
        <code> .git</code> HEAD/index/worktree stay untouched.
      </p>
      <div className="spork-field">
        <label htmlFor="commit-branch">Branch</label>
        <input id="commit-branch" value={branch} onChange={(e) => setBranch(e.target.value)} autoFocus />
      </div>
    </Modal>
  );
}

/** Push — GIT_PUSH (net.connect-gated). */
function PushModal({ nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const [remote, setRemote] = useState("origin");
  const [busy, setBusy] = useState(false);

  async function push(): Promise<void> {
    setBusy(true);
    const res = await run(
      { command: "GIT_PUSH", nodeId, remote: remote.trim() || null },
      { nodeId, label: "Push" },
    );
    setBusy(false);
    if (res) close(); // a denial replaces this with the capability modal
  }

  return (
    <Modal
      title="Push to remote"
      icon="upload"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="danger" busy={busy} onClick={() => void push()}>Push</Button>
        </>
      }
    >
      <div className="spork-field">
        <label htmlFor="push-remote">Remote</label>
        <input id="push-remote" value={remote} onChange={(e) => setRemote(e.target.value)} autoFocus />
      </div>
      <div className="spork-warn-box">
        <Icon name="alert-triangle" size={15} />
        <span>
          Needs the <code>net.connect</code> capability (deny-by-default) and uses
          your existing git credentials.
        </span>
      </div>
    </Modal>
  );
}

/** Garbage collection — GC_RUN (dry-run first). */
function GcModal(): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const [dryRun, setDryRun] = useState(true);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<{ objects: number; bytes: number } | null>(null);

  async function gc(): Promise<void> {
    setBusy(true);
    const res = await run({ command: "GC_RUN", dryRun }, { label: "Garbage collection" });
    setBusy(false);
    if (res && res.result === "GC") {
      setReport({ objects: res.reclaimable.length, bytes: res.bytes });
    }
  }

  return (
    <Modal
      title="Garbage collection"
      icon="trash"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Close</Button>
          <Button variant={dryRun ? "secondary" : "danger"} busy={busy} onClick={() => void gc()}>
            {dryRun ? "Preview" : "Run GC"}
          </Button>
        </>
      }
    >
      <label className="spork-row-between" style={{ cursor: "pointer" }}>
        <span>Dry run (preview only)</span>
        <input type="checkbox" checked={dryRun} onChange={(e) => setDryRun(e.target.checked)} />
      </label>
      {report && (
        <div className="spork-warn-box" style={{ background: "var(--bg-raised)", borderColor: "var(--border-strong)", color: "var(--fg)" }}>
          <Icon name="info" size={15} />
          <span>
            {report.objects} objects · {report.bytes} bytes{" "}
            {dryRun ? "reclaimable" : "freed"}.
          </span>
        </div>
      )}
    </Modal>
  );
}

/** Capability denied — honest surface; inline grant is forward-map (P8). */
function CapabilityModal({ capability, action }: { capability: string; action: string }): JSX.Element {
  const close = useClose();
  return (
    <Modal
      title="Action needs a capability"
      icon="alert-triangle"
      onClose={close}
      footer={<Button variant="primary" onClick={close}>Got it</Button>}
    >
      <p className="spork-modal-note">
        <strong>{action}</strong> requires the <code>{capability}</code> capability,
        which is denied by default.
      </p>
      <div className="spork-warn-box">
        <Icon name="alert-triangle" size={15} />
        <span>
          Grant it in your daemon capability config to enable this action.
          One-click inline granting arrives in P8.
        </span>
      </div>
    </Modal>
  );
}
