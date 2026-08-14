from pathlib import Path

catalog = Path('apps/medusa-desktop/src/providerCatalog.ts')
s = catalog.read_text()
old = '''export function modelSupports(
  provider: ProviderCatalogEntry | undefined,
  modelId: string,
  capability: keyof ModelCapabilities,
): boolean {
  const model = provider?.models?.find((candidate) => candidate.id === modelId);
  if (!model) return false;
  const value = model.capabilities[capability];
  return typeof value === "boolean" ? value : value.length > 0;
}
'''
new = '''export type ModelCapabilityState = "supported" | "unsupported" | "unknown";

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
'''
if old not in s:
    raise SystemExit('providerCatalog modelSupports anchor missing')
catalog.write_text(s.replace(old, new, 1))

app = Path('apps/medusa-desktop/src/App.tsx')
s = app.read_text()
old = 'import { loadProviderCatalog, type ProviderCatalogEntry } from "./providerCatalog";'
new = 'import { loadProviderCatalog, profileModelCapabilityState, type ProviderCatalogEntry } from "./providerCatalog";'
if old not in s:
    raise SystemExit('App import anchor missing')
s = s.replace(old, new, 1)
old = '''  const imageCompatibility = useMemo(() => {
    const normalized = provider.trim().toLowerCase();
    if (normalized === "anthropic" || normalized === "openai" || normalized === "chatgpt-oauth") {
      return { supported: true, text: `${model} is configured for image input.` };
    }
    if (normalized === "anthropic-compatible") {
      return { supported: false, text: "This compatible route is text-only unless image support is explicitly configured." };
    }
    return { supported: undefined, text: "Image compatibility will be verified by the runtime before upload." };
  }, [provider, model]);'''
new = '''  const imageCompatibility = useMemo(() => {
    const state = profileModelCapabilityState(
      providerCatalog,
      provider,
      sharedConfiguration?.connection,
      model,
      "image_input",
    );
    if (state === "supported") {
      return { supported: true, text: `${model} is configured for image input.` };
    }
    if (state === "unsupported") {
      return { supported: false, text: `${model} does not advertise image input on this route.` };
    }
    return { supported: undefined, text: "Image compatibility will be verified by the runtime before upload." };
  }, [providerCatalog, provider, model, sharedConfiguration?.connection]);'''
if old not in s:
    raise SystemExit('App image compatibility anchor missing')
app.write_text(s.replace(old, new, 1))

partial_mock_old = '''vi.mock("./providerCatalog", () => ({
  loadProviderCatalog: vi.fn(),
}));'''
partial_mock_new = '''vi.mock("./providerCatalog", async () => {
  const actual = await vi.importActual<typeof import("./providerCatalog")>("./providerCatalog");
  return {
    ...actual,
    loadProviderCatalog: vi.fn(),
  };
});'''
for test_path in [
    Path('apps/medusa-desktop/src/App.test.tsx'),
    Path('apps/medusa-desktop/src/DesktopMiniMax.test.tsx'),
]:
    test_source = test_path.read_text()
    if partial_mock_old not in test_source:
        raise SystemExit(f'provider catalog mock anchor missing in {test_path}')
    test_path.write_text(test_source.replace(partial_mock_old, partial_mock_new, 1))

catalog_tests = Path('apps/medusa-desktop/src/providerCatalog.test.ts')
s = catalog_tests.read_text()
s = s.replace(
    'import { loadProviderCatalog, modelSupports } from "./providerCatalog";',
    'import { loadProviderCatalog, modelSupports, profileModelCapabilityState, type ProviderCatalogEntry } from "./providerCatalog";',
    1,
)
addition = '''

it("resolves OpenAI OAuth image input by profile provider and connection metadata", () => {
  const providers: ProviderCatalogEntry[] = [{
    id: "openai-oauth",
    displayName: "OpenAI account",
    description: "Browser OAuth route",
    connection: "chatgpt-oauth",
    profileProvider: "openai-oauth",
    authMethods: ["oauth"],
    defaultAuth: "oauth",
    defaultModel: "gpt-5.1-codex",
    modelOptions: ["gpt-5.1-codex"],
    models: [{
      id: "gpt-5.1-codex",
      display_name: "GPT-5.1 Codex",
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

  expect(profileModelCapabilityState(providers, "openai-oauth", "chatgpt-oauth", "gpt-5.1-codex", "image_input")).toBe("supported");
  expect(profileModelCapabilityState(providers, "chatgpt-oauth", "chatgpt-oauth", "gpt-5.1-codex", "image_input")).toBe("unknown");
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
'''
catalog_tests.write_text(s + addition)
