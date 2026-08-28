import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { DESKTOP_TOOL_EVENT, type DesktopTool } from "./desktop-tools";
import { ensureBrowserOauth, loadProviderCatalog, startBrowserOauth } from "./providerCatalog";
import {
  closeRuntime,
  cancelRuntime,
  commandSuggestions,
  configureRuntime,
  findWebArtifact,
  loadSharedConfiguration,
  pollRuntime,
  runRuntimeCommand,
  startRuntime,
  submitRuntime,
} from "./runtime";

const providerCatalog = [
  {
    id: "minimax",
    displayName: "MiniMax direct",
    description: "Direct MiniMax route",
    connection: "direct",
    profileProvider: "minimax",
    authMethods: ["api-key"],
    defaultAuth: "api-key",
    defaultModel: "MiniMax-M3",
    modelOptions: ["MiniMax-M3", "MiniMax-M2.7"],
    browserOauth: false,
    discoverModels: false,
    customValues: false,
    credentialConfigured: true,
    currentCustom: false,
  },
  {
    id: "openai",
    displayName: "OpenAI API",
    description: "Official OpenAI API route",
    connection: "openai-api",
    profileProvider: "openai",
    authMethods: ["api-key"],
    defaultAuth: "api-key",
    defaultModel: "gpt-5.1-codex",
    modelOptions: ["gpt-5.1-codex", "gpt-5.1"],
    baseUrl: "https://api.openai.com/v1",
    browserOauth: false,
    discoverModels: false,
    customValues: false,
    credentialConfigured: true,
    currentCustom: false,
  },
  {
    id: "omniroute",
    displayName: "OmniRoute",
    description: "Managed OmniRoute gateway",
    connection: "omniroute",
    profileProvider: "auto/coding",
    authMethods: ["none"],
    defaultAuth: "none",
    defaultModel: "auto/coding",
    modelOptions: ["auto/coding"],
    baseUrl: "http://127.0.0.1:20128/v1",
    browserOauth: false,
    discoverModels: false,
    customValues: false,
    credentialConfigured: true,
    currentCustom: false,
  },
  {
    id: "openai-oauth",
    displayName: "ChatGPT OAuth",
    description: "ChatGPT OAuth through the Codex app-server",
    connection: "chatgpt-oauth",
    profileProvider: "openai-oauth",
    authMethods: ["none"],
    defaultAuth: "none",
    defaultModel: "gpt-5.6-luna",
    modelOptions: [],
    browserOauth: true,
    discoverModels: true,
    customValues: false,
    credentialConfigured: false,
    currentCustom: false,
  },
];

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./providerCatalog", async () => {
  const actual = await vi.importActual<typeof import("./providerCatalog")>("./providerCatalog");
  return {
    ...actual,
    loadProviderCatalog: vi.fn(),
    ensureBrowserOauth: vi.fn(),
    startBrowserOauth: vi.fn(),
  };
});
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
  return {
    ...actual,
    loadSharedConfiguration: vi.fn().mockResolvedValue({
      revision: 0,
      activeProfile: "default",
      connection: "direct",
      provider: "minimax",
      model: "MiniMax-M3",
      effort: "medium",
      auth: "api-key",
      configured: true,
      credentialConfigured: false,
    }),
    startRuntime: vi.fn(),
    closeRuntime: vi.fn(),
    pollRuntime: vi.fn().mockResolvedValue([]),
    commandSuggestions: vi.fn().mockResolvedValue([]),
    submitRuntime: vi.fn(),
    runRuntimeCommand: vi.fn(),
    cancelRuntime: vi.fn(),
    configureRuntime: vi.fn(),
    findWebArtifact: vi.fn(),
    webArtifactPreviewUrl: vi.fn((path: string) => path),
  };
});

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(loadProviderCatalog).mockReset().mockResolvedValue(providerCatalog);
  vi.mocked(ensureBrowserOauth).mockReset().mockResolvedValue(undefined);
  vi.mocked(startBrowserOauth).mockReset();
  vi.mocked(loadSharedConfiguration).mockReset().mockResolvedValue({
    revision: 0,
    activeProfile: "default",
    connection: "direct",
    provider: "minimax",
    model: "MiniMax-M3",
    effort: "medium",
    auth: "api-key",
    configured: true,
    credentialConfigured: false,
  });
  vi.mocked(startRuntime).mockReset();
  vi.mocked(closeRuntime).mockReset().mockResolvedValue(undefined);
  vi.mocked(configureRuntime).mockReset().mockResolvedValue(undefined);
  vi.mocked(findWebArtifact).mockReset().mockResolvedValue(undefined);
  vi.mocked(commandSuggestions).mockReset().mockResolvedValue([]);
  vi.mocked(runRuntimeCommand).mockReset();
  vi.mocked(submitRuntime).mockReset();
  vi.mocked(cancelRuntime).mockReset();
  vi.mocked(pollRuntime).mockReset().mockResolvedValue([]);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("starts a general chat without requiring a project", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await waitFor(() => expect(loadSharedConfiguration).toHaveBeenCalled());
  await waitFor(() => expect(loadProviderCatalog).toHaveBeenCalled());
  await waitFor(() => expect(startRuntime).toHaveBeenCalledWith(undefined));
  expect(window.localStorage.getItem("medusa.desktop.model")).toBeNull();
  expect(screen.getByRole("heading", { name: "Medusa" })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "General chat" })).toBeInTheDocument();
  expect(screen.getByRole("textbox")).toBeEnabled();
  expect(screen.getByText("Medusa policy remains authoritative")).toBeInTheDocument();
});

it("starts the desktop before OAuth preflight completes", async () => {
  vi.mocked(loadSharedConfiguration).mockResolvedValue({
    revision: 0,
    activeProfile: "default",
    connection: "chatgpt-oauth",
    provider: "openai-oauth",
    model: "gpt-5.6-luna",
    effort: "medium",
    auth: "none",
    configured: true,
    credentialConfigured: false,
  });
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-oauth", repo: "" });
  let finishPreflight: () => void = () => undefined;
  vi.mocked(ensureBrowserOauth).mockReturnValue(new Promise<void>((resolve) => {
    finishPreflight = resolve;
  }));

  render(<App />);

  await waitFor(() => expect(startRuntime).toHaveBeenCalledWith(undefined));
  await waitFor(() => expect(configureRuntime).toHaveBeenCalledWith(
    "runtime-oauth",
    expect.objectContaining({ provider: "openai-oauth", model: "gpt-5.6-luna" }),
  ));
  expect(screen.getByRole("textbox")).toBeEnabled();

  finishPreflight();
});

it("keeps the composer usable while initial runtime configuration is pending", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  let resolveConfiguration: () => void = () => undefined;
  vi.mocked(configureRuntime).mockReturnValue(new Promise<undefined>((resolve) => {
    resolveConfiguration = () => resolve(undefined);
  }));
  render(<App />);

  const composer = await screen.findByPlaceholderText("Ask Medusa anything…");
  await waitFor(() => expect(composer).toBeEnabled());
  expect(configureRuntime).toHaveBeenCalledWith(
    "runtime-general",
    expect.objectContaining({ provider: "minimax", model: "MiniMax-M3" }),
  );

  resolveConfiguration();
});

it("keeps the empty chat quiet and starts the composer at one line", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");

  expect(screen.queryByRole("heading", { name: "How can Medusa help?" })).not.toBeInTheDocument();
  expect(screen.queryByText("Ask a question, paste a screenshot, or open a project when you want Medusa to work on files.")).not.toBeInTheDocument();
  expect(screen.queryByText("Shift+Enter for a new line")).not.toBeInTheDocument();
  expect(screen.getByRole("textbox")).toHaveAttribute("rows", "1");
  await waitFor(() => expect(screen.getByRole("textbox")).toHaveFocus());
});

it("grows the composer only when the prompt spans additional lines", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  const composer = await screen.findByRole("textbox");
  Object.defineProperty(composer, "scrollHeight", { configurable: true, value: 84 });

  fireEvent.change(composer, { target: { value: "First line\nSecond line" } });

  await waitFor(() => expect(composer).toHaveStyle({ height: "84px" }));
});

it("keeps Plan and Settings in the central workspace", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");
  fireEvent.click(screen.getByRole("button", { name: "Plan" }));
  const planHeading = screen.getByRole("heading", { name: "Execution plan" });
  expect(planHeading.closest(".workspace")).toBeInTheDocument();
  expect(planHeading.closest(".side-panel")).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  const settingsHeading = screen.getByRole("heading", { name: "Model settings" });
  expect(settingsHeading.closest(".workspace")).toBeInTheDocument();
  expect(settingsHeading.closest(".side-panel")).not.toBeInTheDocument();
});

it("renders provider and model choices from the shared catalog", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  const providerSelect = screen.getByLabelText("Provider");
  expect(screen.getByRole("option", { name: "MiniMax direct" })).toBeInTheDocument();
  expect(screen.getByRole("option", { name: "OpenAI API" })).toBeInTheDocument();
  expect(screen.getByRole("option", { name: "OmniRoute" })).toHaveValue("auto/coding");

  fireEvent.change(providerSelect, { target: { value: "openai" } });
  expect(screen.getByLabelText("Model")).toHaveValue("gpt-5.1-codex");
  expect(screen.getByRole("option", { name: "gpt-5.1" })).toBeInTheDocument();
});

it("signs in through ChatGPT OAuth and replaces the placeholder with discovered models", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(startBrowserOauth).mockResolvedValue(undefined);
  const authenticatedCatalog = providerCatalog.map((entry) => entry.id === "openai-oauth"
    ? { ...entry, modelOptions: ["gpt-5.6-terra"] }
    : entry);
  vi.mocked(loadProviderCatalog)
    .mockResolvedValueOnce(providerCatalog)
    .mockResolvedValue(authenticatedCatalog);
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "openai-oauth" } });
  await waitFor(() => expect(ensureBrowserOauth).toHaveBeenCalledWith("openai-oauth"));
  await waitFor(() => expect(screen.getByRole("button", { name: "Sign in with ChatGPT" })).toBeInTheDocument());
  expect(screen.queryByRole("option", { name: "gpt-5" })).not.toBeInTheDocument();
  expect(screen.getByLabelText("Model")).toHaveValue("gpt-5.6-terra");

  fireEvent.click(screen.getByRole("button", { name: "Sign in with ChatGPT" }));
  await waitFor(() => expect(startBrowserOauth).toHaveBeenCalledWith("openai-oauth"));
  expect(await screen.findByRole("option", { name: "gpt-5.6-terra" })).toBeInTheDocument();
  expect(screen.getByLabelText("Model")).toHaveValue("gpt-5.6-terra");
});

it("uses catalog auth metadata instead of a frontend provider allowlist", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "auto/coding" } });
  expect(screen.getByLabelText("API key")).toBeDisabled();
  expect(screen.getByLabelText("API key")).toHaveAttribute(
    "placeholder",
    "This route does not require an API key",
  );
});

it("persists the composer provider, model, and effort until changed and applies them before the next turn", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  render(<App />);

  await screen.findByRole("textbox");
  fireEvent.click(screen.getByRole("button", { name: /Choose provider, model, and effort/ }));
  fireEvent.change(screen.getByLabelText("Composer provider"), { target: { value: "openai" } });
  await waitFor(() => expect(screen.getByLabelText("Composer model")).toHaveValue("gpt-5.1-codex"));
  fireEvent.change(screen.getByLabelText("Composer effort"), { target: { value: "high" } });

  expect(screen.getByRole("button", { name: /Choose provider, model, and effort/ })).toHaveTextContent("OpenAI API · gpt-5.1-codex · High");
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Use the selected route" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  await waitFor(() => expect(submitRuntime).toHaveBeenCalled());
  expect(configureRuntime).toHaveBeenCalledWith(
    "runtime-general",
    expect.objectContaining({
      provider: "openai",
      model: "gpt-5.1-codex",
      effort: "high",
      expectedRevision: 0,
    }),
  );
  expect(vi.mocked(configureRuntime).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(submitRuntime).mock.invocationCallOrder[0]);
  expect(screen.getByRole("button", { name: /Choose provider, model, and effort/ })).toHaveTextContent("OpenAI API · gpt-5.1-codex · High");
});

it("closes the composer selector when clicking outside it", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");
  fireEvent.click(screen.getByRole("button", { name: /Choose provider, model, and effort/ }));
  expect(screen.getByRole("dialog", { name: "Provider, model, and effort" })).toBeInTheDocument();

  fireEvent.pointerDown(document.body);
  expect(screen.queryByRole("dialog", { name: "Provider, model, and effort" })).not.toBeInTheDocument();
});

it("gives the icon-only send button an accessible name", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  const composer = await screen.findByRole("textbox");
  const sendButton = screen.getByRole("button", { name: "Send" });
  expect(sendButton).toBeDisabled();

  fireEvent.change(composer, { target: { value: "Hello" } });
  expect(sendButton).toBeEnabled();
});

it("copies a conversation message from its hover control", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(window.navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Hello from the user" } });
  fireEvent.keyDown(composer, { key: "Enter", shiftKey: false });

  await waitFor(() => expect(document.querySelector(".message.user")).toBeTruthy());
  const message = document.querySelector(".message.user") as HTMLElement;
  const copyButton = within(message).getByRole("button", { name: "Copy message" });
  fireEvent.mouseEnter(message);
  fireEvent.click(copyButton);

  await waitFor(() => expect(writeText).toHaveBeenCalledWith("Hello from the user"));
  expect(screen.getByRole("button", { name: "Copied message" })).toBeInTheDocument();
});

it("switches to the stop button while Enter submission is still in flight", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  let resolveSubmit: (value: "started" | "queued") => void = () => undefined;
  vi.mocked(submitRuntime).mockReturnValue(new Promise((resolve) => { resolveSubmit = resolve; }));
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Hello" } });
  fireEvent.keyDown(composer, { key: "Enter", shiftKey: false });

  expect(await screen.findByRole("button", { name: "Stop active turn" })).toBeInTheDocument();
  expect(composer).toHaveValue("");
  resolveSubmit("started");
  await waitFor(() => expect(screen.getByRole("textbox")).toHaveValue(""));
});

it("uses a stop square while working and re-enables send for steering input", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "started" }])
    .mockResolvedValue([]);
  render(<App />);

  const composer = await screen.findByRole("textbox");
  expect(await screen.findByRole("button", { name: "Stop active turn" })).toBeInTheDocument();

  fireEvent.change(composer, { target: { value: "Steer the current turn" } });
  expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => expect(submitRuntime).toHaveBeenCalled());
  expect(screen.getByRole("button", { name: "Stop active turn" })).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Stop active turn" }));
  await waitFor(() => expect(cancelRuntime).toHaveBeenCalledWith("runtime-general"));
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
});

it.each([
  { platform: "Win32", label: "Ctrl+N", modifier: { ctrlKey: true } },
  { platform: "Linux x86_64", label: "Ctrl+N", modifier: { ctrlKey: true } },
  { platform: "MacIntel", label: "⌘N", modifier: { metaKey: true } },
])("uses $label to create a new session on $platform", async ({ platform, label, modifier }) => {
  vi.spyOn(window.navigator, "platform", "get").mockReturnValue(platform);
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");
  expect(screen.getByText(label)).toBeInTheDocument();

  fireEvent.keyDown(document, { key: "n", ...modifier });
  await waitFor(() => expect(runRuntimeCommand).toHaveBeenCalledWith("runtime-general", "/new"));
});

it("closes a newly started runtime when shared configuration is rejected", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-orphan", repo: "" });
  vi.mocked(configureRuntime).mockRejectedValueOnce(new Error("configuration rejected"));

  render(<App />);

  await waitFor(() =>
    expect(closeRuntime).toHaveBeenCalledWith("runtime-orphan"),
  );
  expect(screen.getByText(/configuration rejected/i)).toBeInTheDocument();
});

it("keeps the composer available when the daemon is still starting", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-starting", repo: "" });
  vi.mocked(configureRuntime).mockRejectedValueOnce(new Error("dependency unavailable: daemon endpoint is still starting"));

  render(<App />);

  const composer = await screen.findByRole("textbox");
  await waitFor(() => expect(composer).toBeEnabled());
  expect(closeRuntime).not.toHaveBeenCalledWith("runtime-starting");
  expect(screen.getByText(/daemon endpoint is still starting/i)).toBeInTheDocument();
});

it("presents API keys as persistent OS-managed credentials", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  expect(screen.getByText("Credentials are retained in the appropriate local credential store")).toBeInTheDocument();
  expect(screen.getByLabelText("API key")).toHaveAttribute("placeholder", "Leave blank to use the saved key");
  expect(screen.queryByText(/session-only/i)).not.toBeInTheDocument();
});

it("accepts slash suggestions with Enter and shows skills as selectable options", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(commandSuggestions).mockImplementation(async (_runtimeId, input) => {
    if (input === "/") {
      return [{ name: "skills", usage: "/skills [name]", description: "list installed skills" }];
    }
    if (input === "/skills ") {
      return [{ name: "release", usage: "/release [task]", description: "project skill - Prepare a release" }];
    }
    return [];
  });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  const composer = screen.getByRole("textbox");
  fireEvent.change(composer, { target: { value: "/" } });
  expect(await screen.findByRole("option", { name: /skills/i })).toBeInTheDocument();
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer).toHaveValue("/skills ");

  expect(await screen.findByRole("option", { name: /release/i })).toBeInTheDocument();
  fireEvent.keyDown(composer, { key: "Enter" });
  expect(composer).toHaveValue("/release ");
  expect(runRuntimeCommand).not.toHaveBeenCalled();
});

it("focuses Approve by default and renders conversation URLs as Ctrl-click links", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([
      {
        type: "question",
        prompts: [{
          header: "Permission",
          question: "Allow this exact write?",
          options: [
            { label: "Approve", description: "Allow once" },
            { label: "Deny", description: "Do not run" },
            { label: "Provide feedback", description: "Type feedback" },
          ],
          multiSelect: false,
        }],
      },
      { type: "assistantText", text: "See https://example.com/docs for details." },
    ])
    .mockResolvedValue([]);
  render(<App />);

  const approve = await screen.findByRole("button", { name: /Approve/i });
  expect(approve).toHaveFocus();
  expect(approve).toHaveAttribute("aria-keyshortcuts", "Y");
  expect(screen.getByRole("button", { name: /Deny/i })).toHaveAttribute("aria-keyshortcuts", "N");
  const link = await screen.findByRole("link", { name: "https://example.com/docs" });
  expect(link).toHaveAttribute("title", "Ctrl+click to open");
});

it("keeps diagnostics behind an explicit Session details control", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");
  const details = screen.getByRole("button", { name: "Session details" });
  expect(details).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByRole("complementary", { name: "Session details" })).not.toBeInTheDocument();

  fireEvent.click(details);
  expect(details).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByRole("complementary", { name: "Session details" })).toBeInTheDocument();
});

it("shows a Codex-authenticated OAuth account as ready in Session details", async () => {
  vi.mocked(loadSharedConfiguration).mockResolvedValue({
    revision: 13,
    activeProfile: "default",
    connection: "chatgpt-oauth",
    provider: "openai-oauth",
    model: "gpt-5.6-luna",
    effort: "medium",
    auth: "none",
    baseUrl: undefined,
    configured: true,
    credentialConfigured: true,
  });
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-oauth", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{
      type: "settings",
      model: "gpt-5.6-luna",
      effort: "medium",
      planMode: false,
      credentialConfigured: false,
    }])
    .mockResolvedValue([]);

  render(<App />);

  await screen.findByRole("textbox");
  await waitFor(() => expect(pollRuntime).toHaveBeenCalled());
  fireEvent.click(screen.getByRole("button", { name: "Session details" }));

  const panel = await screen.findByRole("complementary", { name: "Session details" });
  expect(within(panel).getByText("Ready")).toBeInTheDocument();
  expect(within(panel).queryByText("Missing")).not.toBeInTheDocument();
});

it("renders normalized usage metrics from the runtime event", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{
      type: "usage",
      inputTokens: 11,
      outputTokens: 7,
      cacheReadInputTokens: 3,
      cacheCreationInputTokens: 2,
      totalTokens: 23,
      durationMs: 900,
      tokensPerSecondMilli: 25_555,
      estimatedCostMicrousd: 123,
      provenance: "provider_reported",
    }])
    .mockResolvedValue([]);
  render(<App />);

  await screen.findByRole("textbox");
  fireEvent.click(screen.getByRole("button", { name: "Session details" }));

  const panel = await screen.findByRole("complementary", { name: "Session details" });
  await waitFor(() => {
    expect(within(panel).getByText("11")).toBeInTheDocument();
    expect(within(panel).getByText("7")).toBeInTheDocument();
    expect(within(panel).getByText("3")).toBeInTheDocument();
    expect(within(panel).getByText("23")).toBeInTheDocument();
    expect(within(panel).getByText("Model time: 0.9s")).toBeInTheDocument();
  });
});

it("consolidates desktop tools in the session rail", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await screen.findByRole("textbox");
  expect(screen.getByRole("button", { name: "Sessions" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Review changes" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Memory" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Learning" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Engineering" })).toBeInTheDocument();
});

it("routes every Tools rail item through the shared desktop-tool event", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  const requested: DesktopTool[] = [];
  const onToolRequest = (event: Event) => requested.push((event as CustomEvent<DesktopTool>).detail);
  window.addEventListener(DESKTOP_TOOL_EVENT, onToolRequest);
  render(<App />);

  await screen.findByRole("textbox");
  for (const name of ["Sessions", "Review changes", "Memory", "Learning", "Engineering"]) {
    fireEvent.click(screen.getByRole("button", { name }));
  }

  expect(requested).toEqual(["sessions", "review", "memory", "learning", "engineering"]);
  window.removeEventListener(DESKTOP_TOOL_EVENT, onToolRequest);
});

it("renders tool work as collapsed activity rows", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{
      type: "activity",
      activity: { id: "tool-1", kind: "tool", title: "Inspect repository", details: ["private command output"] },
    }])
    .mockResolvedValue([]);
  render(<App />);

  const summary = await screen.findByText("Inspect repository");
  const row = summary.closest("details");
  expect(row).not.toHaveAttribute("open");
  expect(row).toContainElement(screen.getByText("private command output"));
});

it("keeps an incomplete notice event from crashing the renderer", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "notice", title: "Runtime ready" }])
    .mockResolvedValue([]);
  render(<App />);

  expect(await screen.findByText("Runtime ready")).toBeInTheDocument();
  expect(screen.getByRole("textbox")).toBeInTheDocument();
});

it("summarizes active tool work before exposing collapsed details", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([
      { type: "started" },
      {
        type: "activity",
        activity: { id: "tool-1", kind: "tool", title: "Run focused tests", details: ["PASS src/App.test.tsx"] },
      },
    ])
    .mockResolvedValue([]);
  render(<App />);

  expect(await screen.findByRole("status", { name: /running run focused tests/i })).toBeInTheDocument();
  expect(screen.getByLabelText("Tool progress")).toBeInTheDocument();
});

it("keeps technical error details behind disclosure and retries the last request", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "failed", message: "EACCES: permission denied" }])
    .mockResolvedValue([]);
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Write the sample HTML" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  expect(await screen.findByRole("alert")).toBeInTheDocument();
  expect(screen.getAllByText("EACCES: permission denied").length).toBeGreaterThanOrEqual(1);
  expect(screen.getByText("Show details")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(submitRuntime).toHaveBeenCalledTimes(2));
  expect(screen.getByRole("textbox")).toHaveValue("");
});

it("adds a terminal error summary to the chat transcript", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "failed", message: "provider rejected the request" }])
    .mockResolvedValue([]);
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Build the sample page" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  expect(await screen.findByText(/The request did not complete because the runtime reported an error:/)).toBeInTheDocument();
  expect(screen.getAllByText("provider rejected the request").length).toBeGreaterThanOrEqual(1);
});

it("adds a fallback summary when a turn finishes without assistant text", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "turnFinished" }])
    .mockResolvedValue([]);
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Inspect the repository" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  expect(await screen.findByText("The turn finished successfully, but Medusa did not return a chat summary. Check Work for the execution details.")).toBeInTheDocument();
});

it("shows a rendered result in the shared side panel when a turn reports a failed step", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  vi.mocked(submitRuntime).mockResolvedValue("started");
  vi.mocked(pollRuntime)
    .mockResolvedValueOnce([{ type: "failed", message: "worker step failed" }])
    .mockResolvedValue([]);
  vi.mocked(findWebArtifact).mockResolvedValue({ path: "C:/work/index.html", title: "Rendered result" });
  render(<App />);

  const composer = await screen.findByRole("textbox");
  fireEvent.change(composer, { target: { value: "Build the page" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  expect(await screen.findByRole("complementary", { name: "Rendered webpage" })).toBeInTheDocument();
  expect(screen.getByText("Medusa returned a partial result.")).toBeInTheDocument();
  expect(screen.queryByText("Medusa couldn’t complete that request.")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Resize side panel" })).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: /Work/ }));
  expect(screen.getByRole("complementary", { name: "Work" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Session details" }));
  expect(screen.getByRole("complementary", { name: "Session details" })).toBeInTheDocument();
  expect(screen.queryByRole("complementary", { name: "Work" })).not.toBeInTheDocument();
});
