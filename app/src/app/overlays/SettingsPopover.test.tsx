// Settings provider-config tests (P7.5 MVP, W3) — the agent-provider form and the
// model-selector gating helper (docs/MVP_PLAN.md §5 W3).

import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { SettingsPopover } from "./SettingsPopover";
import { availableModels } from "../TopBar";
import { useUiStore, type AgentProvider } from "../../state/store";
import { getAgentConfigs } from "../../ipc/mock";

describe("availableModels (model-selector honesty — no stub)", () => {
  it("offers NOTHING when no local model is detected (no fake default)", () => {
    // The old hardcoded `local/llama3.1` is gone — an empty list means "no model
    // configured", which the UI shows as a configure-a-provider hint.
    expect(availableModels(null)).toEqual([]);
    expect(availableModels(null, [])).toEqual([]);
  });

  it("offers the REAL detected local models (and never a cloud guess)", () => {
    const models = availableModels(null, ["qwen2.5-coder", "llama3.2"]);
    expect(models).toEqual(["local/qwen2.5-coder", "local/llama3.2"]);
    expect(models.some((m) => m.startsWith("anthropic/"))).toBe(false);
    expect(models.some((m) => m.startsWith("openai/"))).toBe(false);
  });

  it("offers only the configured CLI agent for a cli provider", () => {
    const provider: AgentProvider = { kind: "cli", command: "my-cli-agent" };
    expect(availableModels(provider)).toEqual(["cli/my-cli-agent"]);
  });
});

describe("SettingsPopover provider form", () => {
  beforeEach(() => {
    useUiStore.getState().reset();
  });

  it("saves a CLI provider via set_agent_config and updates the store", async () => {
    render(<SettingsPopover />);

    fireEvent.click(screen.getByRole("button", { name: "Conforming CLI" }));
    fireEvent.change(screen.getByLabelText("CLI agent command"), {
      target: { value: "my-cli-agent" },
    });
    fireEvent.change(screen.getByLabelText("CLI agent arguments"), {
      target: { value: "--json" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save provider" }));

    // The choice is sent to the backend (set_agent_config) ...
    await waitFor(() => {
      expect(getAgentConfigs()).toHaveLength(1);
    });
    expect(getAgentConfigs()[0]).toMatchObject({
      kind: "cli",
      command: "my-cli-agent",
      args: ["--json"],
    });

    // ... and reflected in the store, which gates the selector to the CLI agent.
    const state = useUiStore.getState();
    expect(state.agentProvider).toMatchObject({ kind: "cli", command: "my-cli-agent" });
    expect(state.defaultModel).toBe("cli/my-cli-agent");
  });

  it("saves a local endpoint provider", async () => {
    render(<SettingsPopover />);

    fireEvent.change(screen.getByLabelText("Local endpoint URL"), {
      target: { value: "http://127.0.0.1:1234/v1/chat/completions" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save & detect models" }));

    await waitFor(() => {
      expect(getAgentConfigs()).toHaveLength(1);
    });
    expect(getAgentConfigs()[0]).toMatchObject({
      kind: "local",
      endpoint: "http://127.0.0.1:1234/v1/chat/completions",
    });
    // Saving probes the endpoint for its REAL models (mock returns a demo set)
    // and points the default at the first detected one — never a hardcoded guess.
    await waitFor(() => {
      expect(useUiStore.getState().defaultModel).toBe("local/qwen2.5-coder");
    });
    expect(useUiStore.getState().localModels).toEqual(["qwen2.5-coder", "llama3.2"]);
  });
});
