import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { loadProviderCatalog } from "./providerCatalog";
import {
  closeRuntime,
  commandSuggestions,
  configureRuntime,
  loadSharedConfiguration,
  pollRuntime,
  runRuntimeCommand,
  startRuntime,
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
    currentCustom: false,
  },
];

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./providerCatalog", async () => {
  const actual = await vi.importActual<typeof import("./providerCatalog")>("./providerCatalog");
  return {
    ...actual,
    loadProviderCatalog: vi.fn(),
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
  };
});

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(loadProviderCatalog).mockReset().mockResolvedValue(providerCatalog);
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
  vi.mocked(commandSuggestions).mockReset().mockResolvedValue([]);
  vi.mocked(runRuntimeCommand).mockReset();
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

it("gives the icon-only send button an accessible name", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  const composer = await screen.findByRole("textbox");
  const sendButton = screen.getByRole("button", { name: "Send" });
  expect(sendButton).toBeDisabled();

  fireEvent.change(composer, { target: { value: "Hello" } });
  expect(sendButton).toBeEnabled();
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

it("presents API keys as persistent OS-managed credentials", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);
  await waitFor(() => expect(startRuntime).toHaveBeenCalled());

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  expect(screen.getByText("Saved securely in your operating system credential manager")).toBeInTheDocument();
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
