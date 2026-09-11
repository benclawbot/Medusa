import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import {
  ensureBrowserOauth,
  loadProviderCatalog,
  startBrowserOauth,
} from "./providerCatalog";
import {
  configureRuntime,
  loadSharedConfiguration,
  startRuntime,
} from "./runtime";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./providerCatalog", async () => {
  const actual = await vi.importActual<typeof import("./providerCatalog")>(
    "./providerCatalog",
  );
  return {
    ...actual,
    loadProviderCatalog: vi.fn(),
    startBrowserOauth: vi.fn(),
    ensureBrowserOauth: vi.fn(),
  };
});
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>(
    "./runtime",
  );
  return {
    ...actual,
    loadSharedConfiguration: vi.fn(),
    startRuntime: vi.fn(),
    closeRuntime: vi.fn().mockResolvedValue(undefined),
    pollRuntime: vi.fn().mockResolvedValue([]),
    commandSuggestions: vi.fn().mockResolvedValue([]),
    submitRuntime: vi.fn(),
    runRuntimeCommand: vi.fn(),
    cancelRuntime: vi.fn(),
    configureRuntime: vi.fn(),
  };
});

const minimaxEntry = {
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
  discoverModels: true,
  customValues: false,
  currentCustom: false,
};

const oauthEntry = {
  id: "openai-oauth",
  displayName: "ChatGPT OAuth",
  description: "ChatGPT OAuth through the Codex app-server",
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
};

const catalog = [minimaxEntry, oauthEntry];

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(loadProviderCatalog)
    .mockReset()
    .mockResolvedValue(catalog.map((entry) => ({ ...entry })));
  vi.mocked(startBrowserOauth).mockReset().mockResolvedValue(undefined);
  vi.mocked(ensureBrowserOauth).mockReset().mockResolvedValue(undefined);
  vi.mocked(loadSharedConfiguration).mockReset().mockResolvedValue({
    revision: 7,
    activeProfile: "default",
    connection: "direct",
    provider: "minimax",
    model: "MiniMax-M3",
    effort: "medium",
    auth: "api-key",
    configured: true,
    credentialConfigured: true,
  });
  vi.mocked(startRuntime).mockReset().mockResolvedValue({
    runtimeId: "desktop-settings-apply",
    repo: "",
  });
  vi.mocked(configureRuntime).mockReset().mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function openSettings() {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
  await screen.findByRole("button", { name: "Apply configuration" });
}

it("keeps Apply enabled when typing a new MiniMax API key", async () => {
  await openSettings();
  fireEvent.change(screen.getByLabelText("API key"), {
    target: { value: "brand-new-minimax-key" },
  });
  expect(
    screen.getByRole("button", { name: "Apply configuration" }),
  ).toBeEnabled();
});

it("re-enables Apply after an abandoned browser sign-in", async () => {
  let releaseSignIn!: () => void;
  vi.mocked(startBrowserOauth).mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        releaseSignIn = resolve;
      }),
  );
  await openSettings();

  fireEvent.change(screen.getByLabelText("Provider"), {
    target: { value: "openai-oauth" },
  });
  await screen.findByRole("button", { name: "Sign in with ChatGPT" });
  fireEvent.click(
    screen.getByRole("button", { name: "Sign in with ChatGPT" }),
  );
  await screen.findByRole("button", { name: "Opening ChatGPT sign-in…" });

  // The user gives up on the browser flow and switches back to MiniMax
  // while sign-in is still in flight.
  fireEvent.change(screen.getByLabelText("Provider"), {
    target: { value: "minimax" },
  });
  fireEvent.change(screen.getByLabelText("API key"), {
    target: { value: "brand-new-minimax-key" },
  });

  // The orphaned sign-in settles late; it must not pin the UI.
  releaseSignIn();
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Apply configuration" }),
    ).toBeEnabled(),
  );
});
