import { describe, expect, it } from "vitest";
import type { ProviderCatalogEntry } from "./providerCatalog";
import type { SharedConfiguration } from "./runtime";
import {
  configurationIsUsable,
  initialOnboardingStep,
  recommendedModels,
} from "./onboarding";

const provider: ProviderCatalogEntry = {
  id: "openai",
  displayName: "OpenAI",
  description: "OpenAI",
  connection: "openai",
  profileProvider: "openai",
  authMethods: ["api-key"],
  defaultAuth: "api-key",
  defaultModel: "gpt-5",
  modelOptions: ["gpt-5", "gpt-5-mini"],
  browserOauth: false,
  discoverModels: true,
  customValues: false,
  currentCustom: false,
  models: [
    {
      id: "gpt-5",
      display_name: "GPT-5",
      provider_id: "openai",
      profile_provider: "openai",
      availability: "available",
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
    },
  ],
};

function config(overrides: Partial<SharedConfiguration> = {}): SharedConfiguration {
  return {
    revision: 1,
    activeProfile: "default",
    connection: "openai",
    provider: "openai",
    model: "gpt-5",
    effort: "medium",
    auth: "api-key",
    configured: true,
    credentialConfigured: true,
    ...overrides,
  };
}

describe("Desktop onboarding state authority", () => {
  it("bypasses onboarding for an existing usable shared profile", () => {
    expect(configurationIsUsable(config(), [provider])).toBe(true);
    expect(initialOnboardingStep(config(), [provider])).toBe("ready");
  });

  it("resumes partial configuration at authentication", () => {
    const partial = config({ configured: false, credentialConfigured: false });
    expect(configurationIsUsable(partial, [provider])).toBe(false);
    expect(initialOnboardingStep(partial, [provider])).toBe("authentication");
  });

  it("resumes at model selection after authentication", () => {
    const partial = config({ configured: false, model: "" });
    expect(initialOnboardingStep(partial, [provider])).toBe("model");
  });

  it("places recommended rich-registry models first", () => {
    expect(recommendedModels(provider)).toEqual(["gpt-5", "gpt-5-mini"]);
  });

  it("rejects disabled provider configuration", () => {
    expect(configurationIsUsable(config(), [{ ...provider, disabledReason: "Unavailable" }])).toBe(false);
    expect(initialOnboardingStep(config(), [{ ...provider, disabledReason: "Unavailable" }])).toBe("provider");
  });
});
