import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import { loadProviderCatalog, modelSupports, profileModelCapabilityState, type ProviderCatalogEntry } from "./providerCatalog";

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

it("never exposes the OAuth placeholder when discovery is unavailable", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce([{
      id: "openai-oauth",
      displayName: "ChatGPT OAuth",
      description: "Browser OAuth route",
      connection: "chatgpt-oauth",
      profileProvider: "openai-oauth",
      authMethods: ["none"],
      defaultAuth: "none",
      defaultModel: "gpt-5.6-luna",
      modelOptions: ["gpt-5.6-luna"],
      browserOauth: true,
      discoverModels: true,
      customValues: false,
      currentCustom: false,
    }])
    .mockResolvedValueOnce({
      provider_id: "openai-oauth",
      discovery_available: true,
      used_cached_discovery: false,
      models: [{
        id: "gpt-5.6-luna",
        display_name: "gpt-5.6-luna",
        provider_id: "openai-oauth",
        profile_provider: "openai-oauth",
        availability: "not-authorized",
        source: "curated",
        capabilities: {
          tool_calling: true,
          image_input: true,
          audio_input: true,
          realtime: false,
          reasoning: true,
          reasoning_effort_levels: ["low", "medium", "high"],
          streaming: true,
          structured_output: true,
          prompt_caching: true,
        },
        deprecated: false,
        recommended: true,
      }],
    });

  const providers = await loadProviderCatalog(true, "openai-oauth");
  expect(providers[0].modelOptions).toEqual([]);
});

it("uses only account-discovered OAuth models after sign-in", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce([{
      id: "openai-oauth",
      displayName: "ChatGPT OAuth",
      description: "Browser OAuth route",
      connection: "chatgpt-oauth",
      profileProvider: "openai-oauth",
      authMethods: ["none"],
      defaultAuth: "none",
      defaultModel: "gpt-5.6-luna",
      modelOptions: ["gpt-5.6-luna"],
      browserOauth: true,
      discoverModels: true,
      customValues: false,
      currentCustom: false,
    }])
    .mockResolvedValueOnce({
      provider_id: "openai-oauth",
      discovery_available: true,
      used_cached_discovery: false,
      models: [{
        id: "gpt-5.6-terra",
        display_name: "GPT-5.6 Terra",
        provider_id: "openai-oauth",
        profile_provider: "openai-oauth",
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
      }],
    });

  const providers = await loadProviderCatalog(true, "openai-oauth");
  expect(providers[0].modelOptions).toEqual(["gpt-5.6-terra"]);
});


it("resolves OpenAI OAuth image input by profile provider and connection metadata", () => {
  const providers: ProviderCatalogEntry[] = [{
    id: "openai-oauth",
    displayName: "OpenAI account",
    description: "Browser OAuth route",
    connection: "chatgpt-oauth",
    profileProvider: "openai-oauth",
    authMethods: ["none"],
    defaultAuth: "none",
    defaultModel: "gpt-5.6-luna",
    modelOptions: ["gpt-5.6-luna"],
    models: [{
      id: "gpt-5.6-luna",
      display_name: "GPT-5.6 Luna",
      provider_id: "openai-oauth",
      profile_provider: "openai-oauth",
      availability: "available",
      source: "curated",
      capabilities: {
        tool_calling: true,
        image_input: true,
        audio_input: false,
        realtime: false,
        reasoning: true,
        reasoning_effort_levels: ["medium"],
        streaming: true,
        structured_output: true,
        prompt_caching: true,
      },
      deprecated: false,
      recommended: true,
    }],
    browserOauth: true,
    discoverModels: true,
    customValues: false,
    currentCustom: false,
  }];

  expect(profileModelCapabilityState(providers, "openai-oauth", "chatgpt-oauth", "gpt-5.6-luna", "image_input")).toBe("supported");
  expect(profileModelCapabilityState(providers, "chatgpt-oauth", "chatgpt-oauth", "gpt-5.6-luna", "image_input")).toBe("unknown");
});

it("keeps missing model capability metadata unknown instead of treating it as unsupported", () => {
  const providers: ProviderCatalogEntry[] = [{
    id: "anthropic-compatible",
    displayName: "Compatible endpoint",
    description: "Custom route",
    connection: "anthropic-compatible",
    profileProvider: "anthropic-compatible",
    authMethods: ["none"],
    defaultAuth: "none",
    defaultModel: "custom-model",
    modelOptions: ["custom-model"],
    browserOauth: false,
    discoverModels: false,
    customValues: true,
    currentCustom: false,
  }];

  expect(profileModelCapabilityState(providers, "anthropic-compatible", "anthropic-compatible", "custom-model", "image_input")).toBe("unknown");
});
