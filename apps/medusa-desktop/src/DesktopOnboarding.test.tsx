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
  authMethods: ["none"],
  defaultAuth: "none",
  defaultModel: "gpt-5.6-luna",
  modelOptions: ["gpt-5.6-luna"],
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
  model: "gpt-5.6-luna",
  effort: "medium",
  auth: "none",
  configured: false,
  credentialConfigured: false,
};

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("explains approval-gated permissions before first provider use", () => {
  render(<DesktopOnboarding configuration={configuration} providers={[oauthProvider]} onApply={vi.fn()} />);

  expect(screen.getByRole("note")).toHaveTextContent("Ask for approval");
  expect(screen.getByRole("note")).toHaveTextContent("using the internet");
  expect(screen.getByRole("note")).toHaveTextContent("different permission profile after setup");
});

it("uses browser OAuth then refreshes authenticated models", async () => {
  vi.mocked(startBrowserOauth).mockResolvedValue(undefined);
  vi.mocked(loadProviderCatalog).mockResolvedValue([oauthProvider]);
  render(<DesktopOnboarding configuration={configuration} providers={[oauthProvider]} onApply={vi.fn()} />);

  expect(screen.getByRole("heading", { name: "Authenticate" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Sign in with ChatGPT" }));

  await waitFor(() => expect(startBrowserOauth).toHaveBeenCalledWith("openai-oauth"));
  expect(loadProviderCatalog).toHaveBeenCalledWith(true, "openai-oauth");
  expect(await screen.findByRole("heading", { name: "Choose a model" })).toBeInTheDocument();
});

it("keeps authentication recoverable when browser OAuth fails or is cancelled", async () => {
  vi.mocked(startBrowserOauth).mockRejectedValue(new Error("Browser sign-in cancelled"));
  render(<DesktopOnboarding configuration={configuration} providers={[oauthProvider]} onApply={vi.fn()} />);

  fireEvent.click(screen.getByRole("button", { name: "Sign in with ChatGPT" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Browser sign-in cancelled");
  expect(screen.getByRole("heading", { name: "Authenticate" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sign in with ChatGPT" })).toBeEnabled();
});

it("does not treat an OAuth route as authenticated merely because it has no API key", async () => {
  const catalogOAuthProvider = { ...oauthProvider, authMethods: ["none"], defaultAuth: "none" };
  const catalogConfiguration = { ...configuration, auth: "none" };
  render(<DesktopOnboarding configuration={catalogConfiguration} providers={[catalogOAuthProvider]} onApply={vi.fn()} />);

  expect(screen.getByRole("heading", { name: "Authenticate" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sign in with ChatGPT" })).toBeEnabled();
});

it("stages a custom endpoint only on the advanced custom route", async () => {
  const customProvider: ProviderCatalogEntry = {
    id: "anthropic-compatible",
    displayName: "Compatible endpoint",
    description: "Custom-compatible endpoint",
    connection: "anthropic-compatible",
    profileProvider: "anthropic-compatible",
    authMethods: ["none"],
    defaultAuth: "none",
    defaultModel: "custom-model",
    modelOptions: ["custom-model"],
    baseUrl: "https://default.example/v1",
    browserOauth: false,
    discoverModels: false,
    customValues: true,
    currentCustom: false,
  };
  const customConfiguration: SharedConfiguration = {
    ...configuration,
    connection: "anthropic-compatible",
    provider: "anthropic-compatible",
    model: "",
    auth: "none",
    configured: false,
    credentialConfigured: true,
  };
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={customConfiguration} providers={[customProvider]} onApply={onApply} />);
  fireEvent.change(screen.getByLabelText("Model"), { target: { value: "custom-model" } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://gateway.example/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));
  await waitFor(() => expect(onApply).toHaveBeenCalledWith(expect.objectContaining({
    provider: "anthropic-compatible",
    model: "custom-model",
    baseUrl: "https://gateway.example/v1",
  })));
});
