// Right Node-Details panel (UI_UX_DESIGN.md §5.6, §5.8, §14.5).
//
// v1 tabs are CHANGES + INFO only. Conversation (P6) and Results-detail (P7) are
// forward-map — nothing produces CHAT_TOKENS yet and no command reads the
// ResultEnvelope (the result shows on the canvas as the status badge), so showing
// those tabs would break the honesty contract. They are named in the Info tab's
// forward-map note instead of faked.
//
// Changes does a REAL before/after diff (fixing the old empty-original bug):
// NODE_DIFF gives the changed-path set vs a baseline parent; each file's two
// sides come from BLOB_READ of the parent tree and the node tree.

import { lazy, Suspense, useMemo, useState, type JSX } from "react";
import { useUiStore } from "../state/store";
import { useNodeDiff } from "../state/queries";
import { descriptorFor } from "../canvas/descriptors";
import { dispatch } from "../ipc/client";
import { Icon } from "../ui/icons";
import { IconButton } from "../ui/Button";
import { shortId, humanizeModel, formatMicroUsd, effectiveStatus } from "../ui/format";
import type { Command, NodeView } from "../ipc/types";

const DiffEditor = lazy(async () => {
  const mod = await import("@monaco-editor/react");
  return { default: mod.DiffEditor };
});

type Tab = "changes" | "info";

/** Guess a Monaco language id from a file extension. */
function langFromPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    rs: "rust",
    py: "python",
    json: "json",
    md: "markdown",
    css: "css",
    html: "html",
    toml: "ini",
    yaml: "yaml",
    yml: "yaml",
  };
  return map[ext] ?? "plaintext";
}

/** The right-region details panel for the selected node. */
export function NodeDetails(): JSX.Element {
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const view = useUiStore((s) => s.view);
  const selectNode = useUiStore((s) => s.selectNode);
  const openModal = useUiStore((s) => s.openModal);
  const [tab, setTab] = useState<Tab>("changes");

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;

  if (!node) {
    return (
      <section className="spork-details" aria-label="Node details">
        <div className="spork-details-empty">
          <Icon name="circle-dot" size={22} />
          <p>Select a node to inspect it.</p>
        </div>
      </section>
    );
  }

  const d = descriptorFor(node.kind);
  const eff = effectiveStatus(node.status, node.isStale);

  function copyId(): void {
    try {
      void navigator.clipboard?.writeText(node!.id);
    } catch {
      /* clipboard unavailable (e.g. jsdom) — ignore */
    }
  }

  return (
    <section className="spork-details" aria-label="Node details">
      <header className="spork-details-header">
        <span style={{ color: d.color, display: "inline-flex" }} aria-hidden="true">
          <Icon name={d.iconName} size={16} />
        </span>
        <span className="spork-details-kind">{d.label}</span>
        <button className="spork-details-id" onClick={copyId} title="Copy full id">
          {shortId(node.id)}
          <Icon name="copy" size={11} />
        </button>
        <div style={{ flex: 1 }} />
        <IconButton
          icon="messages-square"
          label="Ask the agent about this node"
          onClick={() => openModal({ kind: "askAgent", nodeId: node.id })}
        />
        <span
          className="spork-status"
          data-status={eff.status}
          data-stale={eff.stale ? "true" : undefined}
        >
          <span className="spork-status-dot" aria-hidden="true" />
          {eff.label}
          {eff.stale && <span className="spork-stale-tag"> · stale</span>}
        </span>
      </header>

      <nav className="spork-tabs" role="tablist" aria-label="Node detail tabs">
        {(["changes", "info"] as const).map((t) => (
          <button
            key={t}
            className="spork-tab"
            role="tab"
            aria-selected={tab === t}
            onClick={() => setTab(t)}
          >
            {t === "changes" ? "Changes" : "Info"}
          </button>
        ))}
      </nav>

      <div className="spork-details-body" role="tabpanel">
        {tab === "changes" ? (
          <ChangesTab node={node} view={view} />
        ) : (
          <InfoTab node={node} onSelectParent={selectNode} />
        )}
      </div>
    </section>
  );
}

/** Changes tab — real before/after diff via NODE_DIFF + paired BLOB_READ. */
function ChangesTab({
  node,
  view,
}: {
  node: NodeView;
  view: { nodes: NodeView[] };
}): JSX.Element {
  const parents = node.parentIds;
  const [baseline, setBaseline] = useState<string | null>(parents[0] ?? null);
  const diff = useNodeDiff(node.id, baseline);
  const [openPath, setOpenPath] = useState<string | null>(null);
  const [original, setOriginal] = useState("");
  const [modified, setModified] = useState("");

  const baselineNode = useMemo(
    () => view.nodes.find((n) => n.id === baseline) ?? null,
    [view.nodes, baseline],
  );

  async function readBlob(treeHash: string | null, path: string): Promise<string> {
    if (!treeHash) return "";
    const res = await dispatch({ command: "BLOB_READ", treeHash, path });
    if (res.result === "BLOB") {
      return new TextDecoder().decode(Uint8Array.from(res.bytes));
    }
    return "";
  }

  async function openFile(path: string): Promise<void> {
    setOpenPath(path);
    const [before, after] = await Promise.all([
      readBlob(baselineNode?.snapshotHash ?? null, path),
      readBlob(node.snapshotHash, path),
    ]);
    setOriginal(before);
    setModified(after);
  }

  const paths = diff.data ?? [];

  return (
    <div className="spork-changes">
      <div className="spork-diff-bar">
        <span>Compare against</span>
        <select
          value={baseline ?? ""}
          onChange={(e) => {
            setBaseline(e.target.value || null);
            setOpenPath(null);
          }}
          aria-label="Diff baseline"
        >
          {parents.length === 0 && <option value="">(root — no parent)</option>}
          {parents.map((p) => (
            <option key={p} value={p}>
              parent {shortId(p)}
            </option>
          ))}
        </select>
      </div>

      {diff.isLoading ? (
        <p className="spork-faint">Computing diff…</p>
      ) : paths.length === 0 ? (
        <p className="spork-faint">No changed paths vs baseline.</p>
      ) : (
        <ul className="spork-paths" aria-label="Changed paths">
          {paths.map((p) => (
            <li key={p}>
              <button
                className={`spork-path${openPath === p ? " spork-path--active" : ""}`}
                data-path={p}
                onClick={() => void openFile(p)}
              >
                <Icon name="file" size={12} />
                <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{p}</span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {openPath !== null && (
        <div className="spork-diff-editor">
          <Suspense fallback={<p className="spork-faint">Loading diff…</p>}>
            <DiffEditor
              height="100%"
              theme="vs-dark"
              original={original}
              modified={modified}
              language={langFromPath(openPath)}
              options={{
                readOnly: true,
                renderSideBySide: false,
                minimap: { enabled: false },
                scrollBeyondLastLine: false,
                fontSize: 12,
              }}
            />
          </Suspense>
        </div>
      )}
    </div>
  );
}

/** Info tab — present view-model fields + an honest forward-map note. */
function InfoTab({
  node,
  onSelectParent,
}: {
  node: NodeView;
  onSelectParent: (id: string) => void;
}): JSX.Element {
  const eff = effectiveStatus(node.status, node.isStale);
  return (
    <>
      <dl className="spork-info">
        <dt>Type</dt>
        <dd>
          {descriptorFor(node.kind).label}{" "}
          <span className="spork-faint">({node.family})</span>
        </dd>
        <dt>Status</dt>
        <dd>
          <span className="spork-status" data-status={eff.status} data-stale={eff.stale ? "true" : undefined}>
            <span className="spork-status-dot" aria-hidden="true" />
            {eff.label}
            {eff.stale && <span className="spork-stale-tag"> · stale</span>}
          </span>
        </dd>
        <dt>Branch</dt>
        <dd>{node.branchId}</dd>
        <dt>Model</dt>
        <dd>{node.model ? humanizeModel(node.model) : "—"}</dd>
        <dt>Cost</dt>
        <dd>
          {node.cost ? (
            <span title={`${node.cost.inputTokens} in / ${node.cost.outputTokens} out tokens`}>
              {formatMicroUsd(node.cost.microUsd)}{" "}
              <span className="spork-faint">
                ({node.cost.inputTokens}↑/{node.cost.outputTokens}↓)
              </span>
            </span>
          ) : (
            <span className="spork-faint">—</span>
          )}
        </dd>
        {node.gate && (
          <>
            <dt>Gate</dt>
            <dd>
              <span
                className="spork-gate"
                data-decision={node.gate.decision}
                title={node.gate.reasons.join("\n")}
              >
                <Icon name="gate" size={11} />
                {node.gate.decision}
                {node.gate.overridden && (
                  <span className="spork-faint"> · overridden</span>
                )}
              </span>
              {node.gate.reasons.length > 0 && (
                <div className="spork-gate-reasons spork-faint">
                  {node.gate.reasons[0]}
                </div>
              )}
            </dd>
          </>
        )}
        <dt>Snapshot</dt>
        <dd>
          {node.ownsSnapshot ? (
            <code title={node.snapshotHash ?? ""}>{shortId(node.snapshotHash)}</code>
          ) : (
            <span className="spork-faint">none (observing)</span>
          )}
        </dd>
        <dt>Parents</dt>
        <dd>
          {node.parentIds.length === 0 ? (
            <span className="spork-faint">root</span>
          ) : (
            node.parentIds.map((p) => (
              <button key={p} className="spork-parent-link" onClick={() => onSelectParent(p)}>
                <Icon name="corner-up-left" size={11} />
                {shortId(p)}
              </button>
            ))
          )}
        </dd>
      </dl>
      <LineagePanel node={node} />
      <p className="spork-fwd-note">
        Live conversation streaming (needs a streaming transport) and the
        effects-log surface are designed but not yet built — see UI_UX_DESIGN.md
        §14. Per-node model + cost (P6), gate verdicts, lineage-aware context,
        handoff docs, and the read-only Lineage/History MCP are live (P7).
      </p>
    </>
  );
}

/**
 * The P7 lineage panel: read-only "expand context" affordances (DESIGN §13.2,
 * §13.5, §13.7). Each button dispatches a read command and renders its inline
 * result — the cache-aligned context's `prefix_hash` + selection trace, the
 * regenerable handoff, and the History MCP tool surface.
 */
function LineagePanel({ node }: { node: NodeView }): JSX.Element {
  const [result, setResult] = useState<{ label: string; lines: string[] } | null>(
    null,
  );
  const [busy, setBusy] = useState(false);

  const run = (label: string, command: Command, render: (data: unknown) => string[]) => {
    setBusy(true);
    dispatch(command)
      .then((res) => {
        const data = res.result === "READ" ? res.data : res;
        setResult({ label, lines: render(data) });
      })
      .catch((e) => setResult({ label, lines: [`error: ${String(e)}`] }))
      .finally(() => setBusy(false));
  };

  return (
    <div className="spork-lineage-panel">
      <div className="spork-lineage-actions">
        <button
          disabled={busy}
          onClick={() =>
            run("Context", { command: "NODE_CONTEXT", nodeId: node.id }, (d) => {
              const ctx = d as {
                prefixHash?: string;
                layerCount?: number;
                selectionTrace?: { reason?: string }[];
              };
              return [
                `prefix_hash ${shortId(ctx.prefixHash ?? "")}`,
                `${ctx.layerCount ?? 0} layers`,
                ...(ctx.selectionTrace ?? [])
                  .slice(0, 3)
                  .map((t) => `· ${t.reason ?? ""}`),
              ];
            })
          }
        >
          Explain context
        </button>
        <button
          disabled={busy}
          onClick={() =>
            run("Handoff", { command: "NODE_HANDOFF", nodeId: node.id }, (d) => {
              const h = d as { summary?: string; files_touched?: { path?: string }[] };
              return [
                h.summary ?? "(no summary)",
                ...(h.files_touched ?? []).map((f) => `· ${f.path ?? ""}`),
              ];
            })
          }
        >
          Handoff
        </button>
        <button
          disabled={busy}
          onClick={() =>
            run(
              "Lineage history",
              {
                command: "HISTORY_QUERY",
                request: {
                  jsonrpc: "2.0",
                  id: 1,
                  method: "tools/call",
                  params: {
                    name: "walk_ancestors",
                    arguments: { nodeId: node.id },
                  },
                },
              },
              (d) => {
                const r = d as { result?: { content?: { text?: string }[] } };
                const text = r.result?.content?.[0]?.text ?? "[]";
                return [`walk_ancestors → ${text.slice(0, 80)}`];
              },
            )
          }
        >
          Lineage history
        </button>
      </div>
      {result && (
        <div className="spork-lineage-result">
          <strong>{result.label}</strong>
          {result.lines.map((l, i) => (
            <div key={i} className="spork-faint">
              {l}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
