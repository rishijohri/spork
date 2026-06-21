// The hero chat — a first-class view of the timeline (REALIGNMENT_PLAN §5b).
//
// Chat and canvas are TWO VIEWS OF THE SAME NODE SUBSTRATE: this surface renders
// the selected **line** as a conversation, and the composer **creates a node**
// (it is not a flat chat scroll). Each turn carries its node identity — kind,
// per-type state, model, cost — because every message IS a node you can open,
// branch from, and replay. The composer picks an agentic mode (Ask / Plan /
// Explore / Work) which sets the node `kind`; sending from a non-tip node starts
// a new line (the daemon's fork-on-divergence). A left spine echoes the lineage.

import { useCallback, useEffect, useMemo, useRef, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { useActions } from "./useActions";
import { deriveLines, lineOfNode } from "../state/lines";
import { descriptorFor } from "../canvas/descriptors";
import { fetchTranscript, type TranscriptTurn } from "../ipc/transcript";
import {
  shortId,
  humanizeModel,
  formatMicroUsd,
  statusBadge,
} from "../ui/format";
import { Icon, type IconName } from "../ui/icons";
import { Button } from "../ui/Button";
import { availableModels } from "./TopBar";
import type { AgentRunIntent, CostView, NodeView } from "../ipc/types";

/** The agentic composer modes — each sets a node kind + maps to an intent. */
const MODES: readonly {
  id: string;
  intent: AgentRunIntent;
  label: string;
  icon: IconName;
  color: string;
  help: string;
}[] = [
  { id: "ask", intent: "ask", label: "Ask", icon: "messages-square", color: "#a78bfa", help: "Ask about this node" },
  { id: "plan", intent: "plan", label: "Plan", icon: "list", color: "#818cf8", help: "Plan a change — nothing is edited" },
  { id: "explore", intent: "analysis", label: "Explore", icon: "search", color: "#38bdf8", help: "Analyze, review, summarize" },
  { id: "work", intent: "change", label: "Work", icon: "pencil", color: "#4ade80", help: "Make a change — creates an Edit node" },
];

/** A node is "conversational" if it carries an agent turn (agentic / edit). */
function isConversational(node: NodeView): boolean {
  return node.kind.startsWith("agent-") || node.kind === "codebase-edit";
}

/** The hero chat region (center view alternative to the canvas). */
export function HeroChat(): JSX.Element {
  const view = useUiStore((s) => s.view);
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const selectNode = useUiStore((s) => s.selectNode);
  const setCenterView = useUiStore((s) => s.setCenterView);
  const defaultModel = useUiStore((s) => s.defaultModel);
  const agentProvider = useUiStore((s) => s.agentProvider);
  const attachAgentNode = useUiStore((s) => s.attachAgentNode);
  const attachEditNode = useUiStore((s) => s.attachEditNode);
  const { run } = useActions();

  const [modeId, setModeId] = useState<string>("ask");
  const [prompt, setPrompt] = useState("");
  const [model, setModel] = useState(defaultModel);
  const [busy, setBusy] = useState(false);
  const [transcripts, setTranscripts] = useState<Record<string, TranscriptTurn[]>>({});
  const scrollRef = useRef<HTMLDivElement>(null);

  const headTarget = view.refs.find((r) => r.name === "HEAD")?.target ?? null;
  const currentLine = useMemo(
    () =>
      (selectedId && lineOfNode(view, selectedId)) ||
      (headTarget && lineOfNode(view, headTarget)) ||
      deriveLines(view)[0] ||
      null,
    [view, selectedId, headTarget],
  );

  // The line's nodes, in order, as the thread.
  const threadNodes = useMemo(() => {
    if (!currentLine) return [];
    const onLine = new Set(currentLine.nodeIds);
    return view.nodes.filter((n) => onLine.has(n.id));
  }, [view.nodes, currentLine]);

  // The send target: the selected node if it is on this line, else the line tip.
  // Sending from a non-tip node starts a new line (fork-on-divergence).
  const tipId = currentLine?.tipNodeId ?? null;
  const targetId =
    selectedId && currentLine?.nodeIds.includes(selectedId) ? selectedId : tipId;
  const willFork = targetId !== null && targetId !== tipId;

  const activeMode = MODES.find((m) => m.id === modeId) ?? MODES[0]!;

  // Fetch transcripts for the line's conversational nodes (parallel, best-effort).
  const loadTranscripts = useCallback(async () => {
    const convo = threadNodes.filter(isConversational);
    const entries = await Promise.all(
      convo.map(async (n) => [n.id, await fetchTranscript(n.id)] as const),
    );
    setTranscripts(Object.fromEntries(entries));
  }, [threadNodes]);

  useEffect(() => {
    void loadTranscripts();
  }, [loadTranscripts]);

  // Keep the thread pinned to the newest turn.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [threadNodes.length, transcripts]);

  async function send(): Promise<void> {
    if (!prompt.trim() || busy || targetId === null) return;
    const text = prompt.trim();
    setBusy(true);
    if (activeMode.intent === "change") {
      const res = await run(
        { command: "NODE_AGENT_EDIT", targetNodeId: targetId, prompt: text, modelKey: model, privacy: "any" },
        { nodeId: targetId, label: "Make change" },
      );
      if (res && res.result === "MUTATION") {
        const ids = res.ids;
        const editId = typeof ids["editNodeId"] === "string" ? (ids["editNodeId"] as string) : null;
        if (editId) {
          attachEditNode(
            optimisticEdit(editId, targetId, ids, model),
            targetId,
          );
          selectNode(editId);
        }
      }
    } else {
      const res = await run(
        { command: "NODE_AGENT_RUN", targetNodeId: targetId, prompt: text, modelKey: model, privacy: "any", intent: activeMode.intent },
        { nodeId: targetId, label: activeMode.label },
      );
      if (res && res.result === "MUTATION") {
        const ids = res.ids;
        const newId = typeof ids["nodeId"] === "string" ? (ids["nodeId"] as string) : null;
        if (newId) {
          attachAgentNode(optimisticContext(newId, ids, model, currentLine?.branchId ?? "main"), targetId);
          selectNode(newId);
        }
      }
    }
    setBusy(false);
    setPrompt("");
    void loadTranscripts();
  }

  if (!currentLine) {
    return (
      <div className="spork-chat" data-testid="hero-chat">
        <div className="spork-chat-empty">
          <Icon name="messages-square" size={26} />
          <h3>No line to converse with yet</h3>
          <p className="spork-muted">Open a project, then start a conversation — every turn becomes a node.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="spork-chat" data-testid="hero-chat" style={{ ["--mode" as string]: activeMode.color }}>
      <header className="spork-chat-head">
        <span className="spork-chat-linemark" aria-hidden="true">
          <Icon name="git-commit" size={14} />
        </span>
        <div className="spork-chat-headtext">
          <span className="spork-chat-linename">{currentLine.label}</span>
          <span className="spork-chat-linesub">
            {currentLine.count} node{currentLine.count === 1 ? "" : "s"} · this chat and the canvas are two views of the same timeline
          </span>
        </div>
        <button
          className="spork-chat-jump"
          onClick={() => setCenterView("canvas")}
          title="See this line on the canvas"
        >
          <Icon name="git-branch" size={13} /> Canvas
        </button>
      </header>

      <div className="spork-chat-thread" ref={scrollRef}>
        <div className="spork-chat-spine" aria-hidden="true" />
        {threadNodes.length === 0 ? (
          <p className="spork-chat-hint spork-muted">This line has no nodes yet — send the first turn below.</p>
        ) : (
          threadNodes.map((n) =>
            isConversational(n) ? (
              <ChatTurn
                key={n.id}
                node={n}
                turns={transcripts[n.id]}
                selected={n.id === selectedId}
                onOpen={() => selectNode(n.id)}
              />
            ) : (
              <ChatEvent key={n.id} node={n} onOpen={() => selectNode(n.id)} selected={n.id === selectedId} />
            ),
          )
        )}
      </div>

      <div className="spork-composer">
        <div className="spork-composer-modes" role="group" aria-label="Agentic mode">
          {MODES.map((m) => (
            <button
              key={m.id}
              className={`spork-mode${m.id === modeId ? " spork-mode--on" : ""}`}
              style={{ ["--mc" as string]: m.color }}
              aria-pressed={m.id === modeId}
              onClick={() => setModeId(m.id)}
              title={m.help}
            >
              <Icon name={m.icon} size={13} />
              {m.label}
            </button>
          ))}
        </div>
        <textarea
          className="spork-composer-input"
          rows={3}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder={`${activeMode.help}…  (⌘↵ to send)`}
          aria-label="Message"
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void send();
          }}
        />
        <div className="spork-composer-foot">
          <label className="spork-composer-model">
            <Icon name="circle-dot" size={12} />
            <select value={model} onChange={(e) => setModel(e.target.value)} aria-label="Model">
              {availableModels(agentProvider).map((m) => (
                <option key={m} value={m}>{humanizeModel(m)}</option>
              ))}
            </select>
          </label>
          <span className="spork-composer-context spork-muted">
            {willFork ? (
              <>
                <Icon name="git-branch" size={12} /> starts a new line from {shortId(targetId)}
              </>
            ) : (
              <>creates a {activeMode.label} node on {currentLine.label}</>
            )}
          </span>
          <div style={{ flex: 1 }} />
          <Button
            variant="primary"
            size="sm"
            busy={busy}
            disabled={!prompt.trim() || targetId === null}
            className="spork-composer-send"
            onClick={() => void send()}
          >
            <Icon name={activeMode.icon} size={13} /> {activeMode.label}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** One conversational node rendered as a message group with its node identity. */
function ChatTurn({
  node,
  turns,
  selected,
  onOpen,
}: {
  node: NodeView;
  turns: TranscriptTurn[] | undefined;
  selected: boolean;
  onOpen: () => void;
}): JSX.Element {
  const d = descriptorFor(node.kind);
  const badge = statusBadge(node);
  return (
    <div className={`spork-turn${selected ? " spork-turn--selected" : ""}`}>
      <span className="spork-turn-node" style={{ background: d.color }} aria-hidden="true">
        <Icon name={d.iconName} size={12} />
      </span>
      <div className="spork-turn-body">
        {turns === undefined ? (
          <p className="spork-turn-text spork-muted">Loading…</p>
        ) : turns.length === 0 ? (
          <p className="spork-turn-text spork-muted">No transcript stored for this {d.label.toLowerCase()} node.</p>
        ) : (
          turns.map((t, i) => (
            <div key={i} className="spork-turn-msg" data-role={t.role}>
              <span className="spork-turn-role">{t.role}</span>
              {t.text && <p className="spork-turn-text">{t.text}</p>}
              {t.tools.map((tool, j) => (
                <p key={j} className="spork-turn-tool spork-muted">↳ {tool}</p>
              ))}
            </div>
          ))
        )}
        <div className="spork-turn-meta">
          <span className="spork-turn-kind" style={{ color: d.color }}>{d.label}</span>
          <span className="spork-turn-state" data-status={badge.tone}>
            <span className="spork-status-dot" aria-hidden="true" />
            {badge.label}
          </span>
          {node.model && <span className="spork-turn-model">◆ {humanizeModel(node.model)}</span>}
          {node.cost && <span className="spork-turn-cost">{formatMicroUsd(node.cost.microUsd)}</span>}
          <button className="spork-turn-open" onClick={onOpen}>open as node →</button>
        </div>
      </div>
    </div>
  );
}

/** A non-conversational node (snapshot / check / gate) as a compact event line. */
function ChatEvent({ node, selected, onOpen }: { node: NodeView; selected: boolean; onOpen: () => void }): JSX.Element {
  const d = descriptorFor(node.kind);
  const badge = statusBadge(node);
  return (
    <button className={`spork-chat-event${selected ? " spork-chat-event--selected" : ""}`} onClick={onOpen}>
      <span style={{ color: d.color, display: "inline-flex" }}><Icon name={d.iconName} size={12} /></span>
      <span className="spork-chat-eventkind">{d.label}</span>
      <span className="spork-turn-state" data-status={badge.tone}>
        <span className="spork-status-dot" aria-hidden="true" />
        {badge.label}
      </span>
      <span className="spork-faint spork-mono" style={{ fontSize: 11 }}>{shortId(node.id)}</span>
    </button>
  );
}

/** Build an optimistic agent-context NodeView from an agent-run reply. */
function optimisticContext(
  id: string,
  ids: Record<string, unknown>,
  model: string,
  branchId: string,
): NodeView {
  const micro = typeof ids["costMicroUsd"] === "number" ? (ids["costMicroUsd"] as number) : 0;
  const cost: CostView = {
    inputTokens: typeof ids["inputTokens"] === "number" ? (ids["inputTokens"] as number) : 0,
    outputTokens: typeof ids["outputTokens"] === "number" ? (ids["outputTokens"] as number) : 0,
    microUsd: micro,
  };
  return {
    id,
    kind: "agent-context",
    family: "context",
    status: "passed",
    isStale: false,
    ownsSnapshot: false,
    snapshotHash: null,
    branchId,
    parentIds: [],
    model: typeof ids["model"] === "string" ? (ids["model"] as string) : model,
    cost,
    gate: null,
    presentationStatus: null,
    lineLabel: null,
    forkedFrom: null,
  };
}

/** Build an optimistic Edit NodeView from an agent-edit reply. */
function optimisticEdit(
  id: string,
  targetId: string,
  ids: Record<string, unknown>,
  model: string,
): NodeView {
  const branchId = typeof ids["branchId"] === "string" ? (ids["branchId"] as string) : "main";
  const micro = typeof ids["costMicroUsd"] === "number" ? (ids["costMicroUsd"] as number) : 0;
  return {
    id,
    kind: "codebase-edit",
    family: "mutating",
    status: "passed",
    isStale: false,
    ownsSnapshot: true,
    snapshotHash: "b3:edit",
    branchId,
    parentIds: [targetId],
    model: typeof ids["model"] === "string" ? (ids["model"] as string) : model,
    cost: { inputTokens: 0, outputTokens: 0, microUsd: micro },
    gate: null,
    presentationStatus: null,
    lineLabel: branchId,
    forkedFrom: ids["forked"] === true ? targetId : null,
  };
}
