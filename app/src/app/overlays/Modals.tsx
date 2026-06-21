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
import { MODELS } from "../TopBar";
import type { AgentRunIntent, CostView, NodeView } from "../../ipc/types";

/** Renders whichever modal the store says is open. */
export function Modals(): JSX.Element | null {
  const modal = useUiStore((s) => s.modal);
  const view = useUiStore((s) => s.view);
  if (!modal) return null;

  const node = (id: string): NodeView | null =>
    view.nodes.find((n) => n.id === id) ?? null;

  switch (modal.kind) {
    case "newBranch":
      return <NewBranchModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
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
      return <AskAgentModal node={node(modal.nodeId)} nodeId={modal.nodeId} />;
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
function AskAgentModal({ node, nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const defaultModel = useUiStore((s) => s.defaultModel);
  const attachAgentNode = useUiStore((s) => s.attachAgentNode);
  const [prompt, setPrompt] = useState("");
  const [model, setModel] = useState(defaultModel);
  const [privacy, setPrivacy] = useState("any");
  const [intent, setIntent] = useState<AgentRunIntent>("ask");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ model: string; cost: CostView } | null>(null);

  async function ask(): Promise<void> {
    if (!prompt.trim()) return;
    setBusy(true);
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
      };
      attachAgentNode(attached, nodeId);
      setResult({ model: attached.model ?? model, cost });
    }
    // A capability denial is routed to the capability modal by `run`; this modal
    // stays open so the prompt is not lost.
  }

  return (
    <Modal
      title="Ask the agent"
      icon="messages-square"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            {result ? "Done" : "Cancel"}
          </Button>
          <Button variant="primary" busy={busy} disabled={!prompt.trim()} onClick={() => void ask()}>
            Ask
          </Button>
        </>
      }
    >
      <p className="spork-modal-note">
        A <strong>read-only</strong> run against <code>{shortId(nodeId)}</code>: the
        answer attaches as an <em>Agent</em> context node by a dotted edge — your
        code is untouched and no branch is forked (DESIGN §6.6).
      </p>
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
        <select id="agent-model" value={model} onChange={(e) => setModel(e.target.value)}>
          {MODELS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
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
          <Icon name="messages-square" size={15} />
          <span>
            Answered by <strong>{humanizeModel(result.model)}</strong> ·{" "}
            {result.cost.inputTokens}↑/{result.cost.outputTokens}↓ tokens ·{" "}
            {formatMicroUsd(result.cost.microUsd)}.
          </span>
        </div>
      )}
    </Modal>
  );
}

function useClose(): () => void {
  return useUiStore((s) => s.closeModal);
}

/** New branch — BRANCH_FORK. */
function NewBranchModal({ node, nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const { run } = useActions();
  const [name, setName] = useState(`branch/${nodeId.slice(-7)}`);
  const [busy, setBusy] = useState(false);

  async function create(): Promise<void> {
    if (!name.trim()) return;
    setBusy(true);
    const res = await run(
      { command: "BRANCH_FORK", fromNodeId: nodeId, name: name.trim() },
      { nodeId, label: "New branch" },
    );
    setBusy(false);
    if (res) close();
  }

  return (
    <Modal
      title="New branch"
      icon="git-branch"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="primary" busy={busy} onClick={() => void create()}>
            Create branch
          </Button>
        </>
      }
    >
      <p className="spork-modal-note">
        Fork a new line of work from {node ? "this node" : "the node"}{" "}
        <code>{shortId(nodeId)}</code>. Metadata-only — zero bytes copied.
      </p>
      <div className="spork-field">
        <label htmlFor="branch-name">Name</label>
        <input
          id="branch-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          onKeyDown={(e) => e.key === "Enter" && void create()}
        />
      </div>
    </Modal>
  );
}

/** Merge — BRANCH_MERGE (3-way; conflicts surface, no half-node). */
function MergeModal({ node, nodeId }: { node: NodeView | null; nodeId: string }): JSX.Element {
  const close = useClose();
  const view = useUiStore((s) => s.view);
  const { run } = useActions();
  const branches = view.refs.filter((r) => r.kind === "Branch");
  const [into, setInto] = useState(branches[0]?.name ?? "main");
  const [busy, setBusy] = useState(false);

  async function merge(): Promise<void> {
    setBusy(true);
    const res = await run(
      { command: "BRANCH_MERGE", intoRef: into, fromNodeId: nodeId, resolution: null },
      { nodeId, label: "Merge" },
    );
    setBusy(false);
    if (res) close();
  }

  return (
    <Modal
      title="Merge"
      icon="git-merge"
      onClose={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>Cancel</Button>
          <Button variant="primary" busy={busy} onClick={() => void merge()}>Merge</Button>
        </>
      }
    >
      <p className="spork-modal-note">
        3-way merge from <code>{node?.branchId ?? shortId(nodeId)}</code> into the
        target branch, using the nearest common ancestor. A clean merge creates a
        Merge node; conflicts surface for resolution with no half-node.
      </p>
      <div className="spork-field">
        <label htmlFor="merge-into">Into branch</label>
        <select id="merge-into" value={into} onChange={(e) => setInto(e.target.value)}>
          {branches.length === 0 && <option value="main">main</option>}
          {branches.map((b) => (
            <option key={b.name} value={b.name}>{b.name}</option>
          ))}
        </select>
      </div>
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
