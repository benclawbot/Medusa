import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { DesktopOnboarding } from "./DesktopOnboarding";
import {
  loadProviderCatalog,
  startBrowserOauth,
  type ProviderCatalogEntry,
} from "./providerCatalog";
import type { SharedConfiguration } from "./runtime";

vi.mock("./providerCatalog", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./providerCatalog")>();
  return {
    ...actual,
    loadProviderCatalog: vi.fn(),
    startBrowserOauth: vi.fn(),
  };
});

const oauthProvider: ProviderCatalogEntry = {
  id: "openai-oauth",
  displayName: "OpenAI account",
  description: "OpenAI OAuth",
  connection: "chatgpt-oauth",
  profileProvider: "openai-oauth",
  authMethods: ["oauth"],
  defaultAuth: "oauth",
  defaultModel: "gpt-5",
  modelOptions: ["gpt-5"],
  browserOauth: true,
  discoverModels: true,
  customValues: false,
  currentCustom: false,
};

const configuration: SharedConfiguration = {
  revision: 0,
  activeProfile: "default",
  connection: "chatgpt-oauth",
  provider: "openai-oauth",
  model: "gpt-5",
  effort: "medium",
  auth: "oauth",
  configured: false,
  credentialConfigured: false,
};

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("uses browser OAuth then refreshes authenticated models", async () => {
  vi.mocked(startBrowserOauth).mockResolvedValue(undefined);
  vi.mocked(loadProviderCatalog).mockResolvedValue([oauthProvider]);
  render(<DesktopOnboarding configuration={configuration} providers={[oauthProvider]} onApply={vi.fn()} />);

  expect(screen.getByRole("heading", { name: "Authenticate" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Sign in with browser" }));

  await waitFor(() => expect(startBrowserOauth).toHaveBeenCalledWith("openai-oauth"));
  expect(loadProviderCatalog).toHaveBeenCalledWith(true);
  expect(await screen.findByRole("heading", { name: "Choose a model" })).toBeInTheDocument();
});

it("keeps authentication recoverable when browser OAuth fails or is cancelled", async () => {
  vi.mocked(startBrowserOauth).mockRejectedValue(new Error("Browser sign-in cancelled"));
  render(<DesktopOnboarding configuration={configuration} providers={[oauthProvider]} onApply={vi.fn()} />);

  fireEvent.click(screen.getByRole("button", { name: "Sign in with browser" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Browser sign-in cancelled");
  expect(screen.getByRole("heading", { name: "Authenticate" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
});
