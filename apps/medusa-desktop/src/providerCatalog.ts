import { invoke } from "@tauri-apps/api/core";

export type ModelAvailability =
  | "available"
  | "not-discovered"
  | "not-authorized"
  | "unsupported"
  | "temporarily-unavailable";

export type ModelSource = "curated" | "discovered" | "cached" | "custom";

export interface ModelCapabilities {
  tool_calling: boolean;
  image_input: boolean;
  audio_input: boolean;
  realtime: boolean;
  reasoning: boolean;
  reasoning_effort_levels: string[];
  streaming: boolean;
  structured_output: boolean;
  prompt_caching: boolean;
}

export interface ModelMetadata {
  id: string;
  display_name: string;
  provider_id: string;
  profile_provider: string;
  availability: ModelAvailability;
  source: ModelSource;
  context_limit?: number;
  output_limit?: number;
  capabilities: ModelCapabilities;
  deprecated: boolean;
  replacement?: string;
  recommended: boolean;
  use_case_hint?: string;
}

export interface ModelRegistry {
  provider_id: string;
  models: ModelMetadata[];
  discovery_available: boolean;
  used_cached_discovery: boolean;
}

export interface ProviderCatalogEntry {
  id: string;
  displayName: string;
  description: string;
  connection: string;
  profileProvider: string;
  authMethods: string[];
  defaultAuth: string;
  defaultModel: string;
  modelOptions: string[];
  models?: ModelMetadata[];
  baseUrl?: string;
  browserOauth: boolean;
  discoverModels: boolean;
  customValues: boolean;
  disabledReason?: string;
  currentCustom: boolean;
}

export async function loadModelRegistry(refresh = false): Promise<ModelRegistry> {
  return invoke<ModelRegistry>("desktop_model_registry", { refresh });
}

export async function loadProviderCatalog(refresh = false): Promise<ProviderCatalogEntry[]> {
  const providers = await invoke<ProviderCatalogEntry[]>("desktop_provider_catalog");
  try {
    const registry = await loadModelRegistry(refresh);
    return providers.map((provider) =>
      provider.id === registry.provider_id || provider.profileProvider === registry.provider_id
        ? {
            ...provider,
            models: registry.models,
            modelOptions: registry.models.map((model) => model.id),
          }
        : provider,
    );
  } catch {
    // Provider metadata remains usable when authenticated discovery is offline.
    return providers;
  }
}

export async function startBrowserOauth(provider: string): Promise<void> {
  await invoke("desktop_browser_oauth", { provider });
}

export type ModelCapabilityState = "supported" | "unsupported" | "unknown";

export function modelCapabilityState(
  provider: ProviderCatalogEntry | undefined,
  modelId: string,
  capability: keyof ModelCapabilities,
): ModelCapabilityState {
  const model = provider?.models?.find((candidate) => candidate.id === modelId);
  if (!model) return "unknown";
  const value = model.capabilities[capability];
  const supported = typeof value === "boolean" ? value : value.length > 0;
  return supported ? "supported" : "unsupported";
}

export function profileModelCapabilityState(
  providers: ProviderCatalogEntry[],
  profileProvider: string,
  connection: string | undefined,
  modelId: string,
  capability: keyof ModelCapabilities,
): ModelCapabilityState {
  const provider = providers.find((candidate) =>
    candidate.profileProvider === profileProvider
    && (!connection || candidate.connection === connection)
  ) ?? providers.find((candidate) => candidate.profileProvider === profileProvider);
  return modelCapabilityState(provider, modelId, capability);
}

export function modelSupports(
  provider: ProviderCatalogEntry | undefined,
  modelId: string,
  capability: keyof ModelCapabilities,
): boolean {
  return modelCapabilityState(provider, modelId, capability) === "supported";
}
