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
new = 'import { loadProviderCatalog, modelCapabilityState, type ProviderCatalogEntry } from "./providerCatalog";'
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
    const selectedProvider = providerCatalog.find((candidate) =>
      candidate.profileProvider === provider
      && (sharedConfiguration?.provider !== provider || candidate.connection === sharedConfiguration.connection)
    ) ?? providerCatalog.find((candidate) => candidate.profileProvider === provider);
    const state = modelCapabilityState(selectedProvider, model, "image_input");
    if (state === "supported") {
      return { supported: true, text: `${model} is configured for image input.` };
    }
    if (state === "unsupported") {
      return { supported: false, text: `${model} does not advertise image input on this route.` };
    }
    return { supported: undefined, text: "Image compatibility will be verified by the runtime before upload." };
  }, [providerCatalog, provider, model, sharedConfiguration?.provider, sharedConfiguration?.connection]);'''
if old not in s:
    raise SystemExit('App image compatibility anchor missing')
app.write_text(s.replace(old, new, 1))

tests = Path('apps/medusa-desktop/src/App.test.tsx')
s = tests.read_text()
addition = '''

it("uses canonical model capability metadata for OpenAI OAuth image input", async () => {
  const imageCapabilities = {
    tool_calling: true,
    image_input: true,
    audio_input: false,
    realtime: false,
    reasoning: true,
    reasoning_effort_levels: ["medium"],
    streaming: true,
    structured_output: true,
    prompt_caching: true,
  };
  vi.mocked(loadSharedConfiguration).mockResolvedValue({ revision: 3, activeProfile: "default", connection: "chatgpt-oauth", provider: "openai-oauth", model: "gpt-5.1-codex", effort: "medium", auth: "oauth", configured: true, credentialConfigured: true });
  vi.mocked(loadProviderCatalog).mockResolvedValue([{
    id: "openai-oauth", displayName: "OpenAI account", description: "Browser OAuth route", connection: "chatgpt-oauth", profileProvider: "openai-oauth", authMethods: ["oauth"], defaultAuth: "oauth", defaultModel: "gpt-5.1-codex", modelOptions: ["gpt-5.1-codex"],
    models: [{ id: "gpt-5.1-codex", display_name: "GPT-5.1 Codex", provider_id: "openai-oauth", profile_provider: "openai-oauth", availability: "available", source: "curated", capabilities: imageCapabilities, deprecated: false, recommended: true }],
    browserOauth: true, discoverModels: true, customValues: false, currentCustom: false,
  }]);
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-oauth", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());
  expect(screen.getByText("gpt-5.1-codex is configured for image input.")).toBeInTheDocument();
});

it("does not confuse a connection identifier with a profile provider identifier", async () => {
  vi.mocked(loadSharedConfiguration).mockResolvedValue({ revision: 3, activeProfile: "default", connection: "chatgpt-oauth", provider: "chatgpt-oauth", model: "gpt-5.1-codex", effort: "medium", auth: "oauth", configured: true, credentialConfigured: true });
  vi.mocked(loadProviderCatalog).mockResolvedValue([{
    id: "openai-oauth", displayName: "OpenAI account", description: "Browser OAuth route", connection: "chatgpt-oauth", profileProvider: "openai-oauth", authMethods: ["oauth"], defaultAuth: "oauth", defaultModel: "gpt-5.1-codex", modelOptions: ["gpt-5.1-codex"], browserOauth: true, discoverModels: true, customValues: false, currentCustom: false,
  }]);
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-oauth", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());
  expect(screen.getByText("Image compatibility will be verified by the runtime before upload.")).toBeInTheDocument();
});

it("keeps custom routes conservative without image capability metadata", async () => {
  vi.mocked(loadSharedConfiguration).mockResolvedValue({ revision: 2, activeProfile: "default", connection: "anthropic-compatible", provider: "anthropic-compatible", model: "custom-model", effort: "medium", auth: "none", configured: true, credentialConfigured: true });
  vi.mocked(loadProviderCatalog).mockResolvedValue([{
    id: "anthropic-compatible", displayName: "Compatible endpoint", description: "Custom route", connection: "anthropic-compatible", profileProvider: "anthropic-compatible", authMethods: ["none"], defaultAuth: "none", defaultModel: "custom-model", modelOptions: ["custom-model"], browserOauth: false, discoverModels: false, customValues: true, currentCustom: false,
  }]);
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-custom", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());
  expect(screen.getByText("Image compatibility will be verified by the runtime before upload.")).toBeInTheDocument();
});
'''
tests.write_text(s + addition)
