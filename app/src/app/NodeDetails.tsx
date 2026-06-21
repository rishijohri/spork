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
import { shortId, humanizeModel, effectiveStatus } from "../ui/format";
import type { NodeView } from "../ipc/types";

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
      <p className="spork-fwd-note">
        Conversation (P6), detailed results &amp; run history (P7), cost, handoff,
        and effects-log surfaces are designed but not yet built — see
        UI_UX_DESIGN.md §14. A check's pass/fail shows on its canvas card today.
      </p>
    </>
  );
}
