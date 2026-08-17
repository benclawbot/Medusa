import type { ProviderCatalogEntry } from "./providerCatalog";
import type { SharedConfiguration } from "./runtime";

export type OnboardingStep = "provider" | "authentication" | "model" | "preferences" | "verify" | "ready";

export interface OnboardingDraft {
  provider: string;
  model: string;
  effort: SharedConfiguration["effort"];
  apiKey: string;
}

export function providerForProfile(
  providers: ProviderCatalogEntry[],
  profileProvider: string,
): ProviderCatalogEntry | undefined {
  return providers.find((entry) => entry.profileProvider === profileProvider);
}

function providerRequiresCredential(provider: ProviderCatalogEntry): boolean {
  return provider.browserOauth || provider.authMethods.some((method) => method !== "none");
}

export function configurationIsUsable(
  configuration: SharedConfiguration | undefined,
  providers: ProviderCatalogEntry[],
): boolean {
  if (!configuration?.configured || !configuration.provider.trim() || !configuration.model.trim()) return false;
  const provider = providerForProfile(providers, configuration.provider);
  if (!provider || provider.disabledReason) return false;
  if (providerRequiresCredential(provider) && !configuration.credentialConfigured) return false;
  return true;
}

export function initialOnboardingStep(
  configuration: SharedConfiguration | undefined,
  providers: ProviderCatalogEntry[],
): OnboardingStep {
  if (!configuration?.provider.trim()) return "provider";
  const provider = providerForProfile(providers, configuration.provider);
  if (!provider || provider.disabledReason) return "provider";
  if (providerRequiresCredential(provider) && !configuration.credentialConfigured) {
    return "authentication";
  }
  if (!configuration.model.trim()) return "model";
  if (!configuration.configured) return "verify";
  return "ready";
}

export function onboardingDraftFromConfiguration(configuration: SharedConfiguration): OnboardingDraft {
  return {
    provider: configuration.provider,
    model: configuration.model,
    effort: configuration.effort,
    apiKey: "",
  };
}

export function recommendedModels(provider: ProviderCatalogEntry | undefined): string[] {
  if (!provider) return [];
  const rich = provider.models ?? [];
  const recommended = rich.filter((model) => model.recommended && !model.deprecated).map((model) => model.id);
  const available = rich
    .filter((model) => model.availability === "available" && !model.deprecated)
    .map((model) => model.id);
  return Array.from(new Set([...recommended, ...available, ...provider.modelOptions]));
}
