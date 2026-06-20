// Right Node-Details panel (DESIGN.md §14.2, §14.5).
//
// Tabs: conversation/chat, a Monaco diff viewer fed lazily via dispatch
// (NodeDiff -> changed paths, BlobRead -> blob bytes), and typed results. The
// diff content is fetched on demand (DESIGN.md §14.5 — O(changed files)). Monaco
// is lazy-loaded so the heavy editor never loads in jsdom tests unless the diff
// tab is opened.

import { lazy, Suspense, useState } from "react";
import { useUiStore } from "../state/store";
import { useNodeDiff } from "../state/queries";
import { descriptorFor } from "../canvas/descriptors";
import { ActionToolbar } from "./ActionToolbar";
import { dispatch } from "../ipc/client";
import type { NodeView } from "../ipc/types";

// Lazy: the Monaco diff editor only loads when the Diff tab mounts.
const DiffEditor = lazy(async () => {
  const mod = await import("@monaco-editor/react");
  return { default: mod.DiffEditor };
});

type Tab = "conversation" | "diff" | "results";

/** The right-region details panel for the selected node. */
export function NodeDetails(): JSX.Element {
  const selectedId = useUiStore((s) => s.selectedNodeId);
  const view = useUiStore((s) => s.view);
  const rail = useUiStore((s) => s.rail);
  const [tab, setTab] = useState<Tab>("conversation");

  const node: NodeView | null =
    (selectedId && view.nodes.find((n) => n.id === selectedId)) || null;

  if (!node) {
    return (
      <section className="spork-details" aria-label="Node details">
        <p className="spork-details-empty">Select a node to see its details.</p>
      </section>
    );
  }

  const descriptor = descriptorFor(node.kind);

  return (
    <section className="spork-details" aria-label="Node details">
      <header className="spork-details-header">
        <span aria-hidden="true">{descriptor.icon}</span>
        <h2 className="spork-details-title">{descriptor.label}</h2>
        <code className="spork-details-id">{node.id}</code>
      </header>

      <ActionToolbar node={node} ariaLabel="Node actions" />

      <nav className="spork-details-tabs" role="tablist">
        {(["conversation", "diff", "results"] as const).map((t) => (
          <button
            key={t}
            role="tab"
            aria-selected={tab === t}
            className={tab === t ? "active" : ""}
            onClick={() => setTab(t)}
          >
            {t}
          </button>
        ))}
      </nav>

      <div className="spork-details-body" role="tabpanel">
        {tab === "conversation" && (
          <ConversationTab lines={rail[node.id] ?? []} />
        )}
        {tab === "diff" && <DiffTab node={node} />}
        {tab === "results" && <ResultsTab node={node} />}
      </div>
    </section>
  );
}

/** Conversation/chat tab — replays buffered ephemeral chat tokens. */
function ConversationTab({ lines }: { lines: string[] }): JSX.Element {
  return (
    <div className="spork-conversation">
      {lines.length === 0 ? (
        <p className="spork-muted">No conversation yet.</p>
      ) : (
        <pre className="spork-conversation-log">{lines.join("")}</pre>
      )}
    </div>
  );
}

/** Diff tab — fetches changed paths + a blob lazily, feeds Monaco. */
function DiffTab({ node }: { node: NodeView }): JSX.Element {
  const diff = useNodeDiff(node.id);
  const [openPath, setOpenPath] = useState<string | null>(null);
  const [blob, setBlob] = useState<string>("");

  async function openFile(path: string): Promise<void> {
    setOpenPath(path);
    if (!node.snapshotHash) {
      setBlob("");
      return;
    }
    const res = await dispatch({
      command: "BLOB_READ",
      treeHash: node.snapshotHash,
      path,
    });
    if (res.result === "BLOB") {
      setBlob(new TextDecoder().decode(Uint8Array.from(res.bytes)));
    }
  }

  return (
    <div className="spork-diff">
      <ul className="spork-diff-paths" aria-label="Changed paths">
        {(diff.data ?? []).map((p) => (
          <li key={p}>
            <button
              data-path={p}
              className={openPath === p ? "active" : ""}
              onClick={() => void openFile(p)}
            >
              {p}
            </button>
          </li>
        ))}
        {diff.data && diff.data.length === 0 && (
          <li className="spork-muted">No changed paths.</li>
        )}
      </ul>
      {openPath !== null && (
        <div className="spork-diff-editor" style={{ height: 240 }}>
          <Suspense fallback={<p className="spork-muted">Loading diff…</p>}>
            <DiffEditor
              height="240px"
              original=""
              modified={blob}
              language="plaintext"
              options={{ readOnly: true, renderSideBySide: true }}
            />
          </Suspense>
        </div>
      )}
    </div>
  );
}

/** Typed results tab — placeholder reading the node's status. */
function ResultsTab({ node }: { node: NodeView }): JSX.Element {
  return (
    <dl className="spork-results">
      <dt>Status</dt>
      <dd>{node.status}</dd>
      <dt>Family</dt>
      <dd>{node.family}</dd>
      <dt>Stale</dt>
      <dd>{node.isStale ? "yes" : "no"}</dd>
      <dt>Owns snapshot</dt>
      <dd>{node.ownsSnapshot ? "yes" : "no"}</dd>
      <dt>Model</dt>
      <dd>{node.model ?? "—"}</dd>
    </dl>
  );
}
