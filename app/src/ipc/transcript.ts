// Shared transcript fetch + parse (P7.5 W5b; reused by the Node-Details Thread
// tab and the hero chat — REALIGNMENT_PLAN §5b).
//
// A node's stored conversation is a `CanonicalTranscript` (content-addressed,
// bound by `conversation_ref`), retrieved through the read-only History MCP
// (`get_node_transcript`) and double-wrapped by the MCP envelope. This module is
// the single place that knows that shape.

import { dispatch } from "./client";
import type { Ulid } from "./types";

/** A flattened conversation turn for rendering. */
export interface TranscriptTurn {
  role: string;
  text: string;
  tools: string[];
}

/** Fetch a node's stored transcript via the History MCP (empty on any failure). */
export async function fetchTranscript(nodeId: Ulid): Promise<TranscriptTurn[]> {
  try {
    const res = await dispatch({
      command: "HISTORY_QUERY",
      request: {
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: "get_node_transcript", arguments: { nodeId } },
      },
    });
    return res.result === "READ" ? parseTranscript(res.data) : [];
  } catch {
    return [];
  }
}

/**
 * Parse the History-MCP `get_node_transcript` response into renderable turns. The
 * stored transcript is a `CanonicalTranscript` (externally-tagged `snake_case`
 * content blocks: `{text}` / `{tool_call}` / `{tool_result}`), double-wrapped by
 * the MCP envelope (`result.content[0].text` → `{transcript}`).
 */
export function parseTranscript(data: unknown): TranscriptTurn[] {
  const result = (data as Record<string, unknown> | null)?.["result"] as
    | Record<string, unknown>
    | undefined;
  const content = result?.["content"] as Array<Record<string, unknown>> | undefined;
  const text = content?.[0]?.["text"];
  if (typeof text !== "string") return [];

  let transcriptJson: string;
  try {
    const wrapper = JSON.parse(text) as { transcript?: unknown };
    if (typeof wrapper.transcript !== "string") return [];
    transcriptJson = wrapper.transcript;
  } catch {
    return [];
  }

  try {
    const parsed = JSON.parse(transcriptJson) as {
      turns?: Array<{ role?: string; content?: Array<Record<string, unknown>> }>;
    };
    if (!Array.isArray(parsed.turns)) return [];
    return parsed.turns.map((turn) => {
      let txt = "";
      const tools: string[] = [];
      for (const block of turn.content ?? []) {
        if (typeof block["text"] === "string") {
          txt += (txt ? "\n" : "") + (block["text"] as string);
        } else if (block["tool_call"]) {
          const tc = block["tool_call"] as { name?: string };
          tools.push(`tool: ${tc.name ?? "?"}`);
        } else if (block["tool_result"]) {
          const tr = block["tool_result"] as { is_error?: boolean };
          tools.push(`tool result${tr.is_error ? " (error)" : ""}`);
        }
      }
      return { role: turn.role ?? "?", text: txt, tools };
    });
  } catch {
    return [];
  }
}
