import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import { loadProviderCatalog, modelSupports } from "./providerCatalog";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

it("replaces active provider string options with canonical model metadata", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce([
      {
        id: "openai",
        displayName: "OpenAI API",
        description: "Official OpenAI API route",
        connection: "openai-api",
        profileProvider: "openai",
        authMethods: ["api-key"],
        defaultAuth: "api-key",
        defaultModel: "gpt-default",
        modelOptions: ["gpt-default"],
        browserOauth: false,
        discoverModels: true,
        customValues: false,
        currentCustom: false,
      },
    ])
    .mockResolvedValueOnce({
      provider_id: "openai",
      discovery_available: true,
      used_cached_discovery: false,
      models: [
        {
          id: "gpt-account-model",
          display_name: "GPT Account Model",
          provider_id: "openai",
          profile_provider: "openai",
          availability: "available",
          source: "discovered",
          capabilities: {
            tool_calling: true,
            image_input: true,
            audio_input: false,
            realtime: false,
            reasoning: true,
            reasoning_effort_levels: ["low", "medium", "high"],
            streaming: true,
            structured_output: true,
            prompt_caching: true,
          },
          deprecated: false,
          recommended: true,
        },
      ],
    });

  const providers = await loadProviderCatalog();
  expect(providers[0].modelOptions).toEqual(["gpt-account-model"]);
  expect(modelSupports(providers[0], "gpt-account-model", "image_input")).toBe(true);
  expect(modelSupports(providers[0], "gpt-account-model", "realtime")).toBe(false);
});

it("keeps deterministic provider fallback when discovery is unavailable", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce([
      {
        id: "minimax",
        displayName: "MiniMax direct",
        description: "Direct MiniMax route",
        connection: "direct",
        profileProvider: "minimax",
        authMethods: ["api-key"],
        defaultAuth: "api-key",
        defaultModel: "MiniMax-M3",
        modelOptions: ["MiniMax-M3"],
        browserOauth: false,
        discoverModels: true,
        customValues: false,
        currentCustom: false,
      },
    ])
    .mockRejectedValueOnce(new Error("offline"));

  const providers = await loadProviderCatalog();
  expect(providers[0].modelOptions).toEqual(["MiniMax-M3"]);
});
