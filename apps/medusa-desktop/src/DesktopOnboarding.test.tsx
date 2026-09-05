import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { DesktopOnboarding } from "./DesktopOnboarding";
import { loadPermissionModes, setPermissionMode } from "./permissionModes";
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

vi.mock("./permissionModes", () => ({
  loadPermissionModes: vi.fn().mockResolvedValue([
    {
      id: "ask-for-approval",
      label: "Ask for approval",
      description: "Ask before protected boundary actions.",
      active: true,
    },
  ]),
  setPermissionMode: vi.fn().mockResolvedValue({
    id: "ask-for-approval",
    label: "Ask for approval",
    description: "Ask before protected boundary actions.",
    active: true,
  }),
}));

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

const localProvider: ProviderCatalogEntry = {
  ...oauthProvider,
  id: "local",
  displayName: "Local",
  connection: "local",
  profileProvider: "local",
  authMethods: ["none"],
  defaultAuth: "none",
  defaultModel: "local-model",
  modelOptions: ["local-model"],
  browserOauth: false,
  discoverModels: false,
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

const localConfiguration: SharedConfiguration = {
  ...configuration,
  connection: "local",
  provider: "local",
  model: "local-model",
  auth: "none",
  credentialConfigured: true,
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

it("defaults first-run permissions to Ask for approval and saves the reviewed choice", async () => {
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={localConfiguration} providers={[localProvider]} onApply={onApply} />);

  expect(screen.getByRole("heading", { name: "Preferences and permissions" })).toBeInTheDocument();
  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("ask-for-approval"));
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(screen.getByText(/Permissions:/)).toHaveTextContent("Ask for approval");
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  await waitFor(() => expect(setPermissionMode).toHaveBeenCalledWith("ask-for-approval"));
  expect(onApply).toHaveBeenCalled();
});

it("preserves an existing persisted permission when onboarding is re-entered", async () => {
  vi.mocked(loadPermissionModes)
    .mockResolvedValueOnce([
      {
        id: "read-only",
        label: "Read Only",
        description: "Read workspace files; ask before edits or internet access.",
        active: true,
      },
    ])
    .mockResolvedValueOnce([
      {
        id: "read-only",
        label: "Read Only",
        description: "Read workspace files; ask before edits or internet access.",
        active: true,
      },
    ]);
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={localConfiguration} providers={[localProvider]} onApply={onApply} />);

  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("read-only"));
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(screen.getByText(/Permissions:/)).toHaveTextContent("Read Only");
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  await waitFor(() => expect(setPermissionMode).toHaveBeenCalledWith("read-only"));
  expect(setPermissionMode).not.toHaveBeenCalledWith("ask-for-approval");
  expect(onApply).toHaveBeenCalled();
});

it("revalidates an untouched permission before saving so an external change wins", async () => {
  vi.mocked(loadPermissionModes)
    .mockResolvedValueOnce([
      {
        id: "full-access",
        label: "Full Access",
        description: "Unrestricted access.",
        active: true,
      },
    ])
    .mockResolvedValueOnce([
      {
        id: "read-only",
        label: "Read Only",
        description: "Read workspace files; ask before edits or internet access.",
        active: true,
      },
    ]);
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={localConfiguration} providers={[localProvider]} onApply={onApply} />);

  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("full-access"));
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(screen.getByText(/Permissions:/)).toHaveTextContent("Full Access");
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  await waitFor(() => expect(setPermissionMode).toHaveBeenCalledWith("read-only"));
  expect(setPermissionMode).not.toHaveBeenCalledWith("full-access");
  expect(loadPermissionModes).toHaveBeenCalledTimes(2);
  expect(onApply).toHaveBeenCalled();
});

it("honors an explicit onboarding permission change without replacing it during revalidation", async () => {
  vi.mocked(loadPermissionModes).mockResolvedValueOnce([
    {
      id: "read-only",
      label: "Read Only",
      description: "Read workspace files; ask before edits or internet access.",
      active: true,
    },
  ]);
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={localConfiguration} providers={[localProvider]} onApply={onApply} />);

  const permissionSelect = await screen.findByLabelText("Model permissions");
  expect(permissionSelect).toHaveValue("read-only");
  fireEvent.change(permissionSelect, { target: { value: "full-access" } });
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  await waitFor(() => expect(setPermissionMode).toHaveBeenCalledWith("full-access"));
  expect(loadPermissionModes).toHaveBeenCalledTimes(1);
  expect(onApply).toHaveBeenCalled();
});

it("surfaces permission persistence failures without applying provider configuration", async () => {
  vi.mocked(setPermissionMode).mockRejectedValueOnce(new Error("permission store unavailable"));
  const onApply = vi.fn().mockResolvedValue(undefined);
  render(<DesktopOnboarding configuration={localConfiguration} providers={[localProvider]} onApply={onApply} />);

  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("ask-for-approval"));
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not save model permissions: permission store unavailable",
  );
  expect(onApply).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Verify configuration" })).toBeEnabled();
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
  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("ask-for-approval"));
  fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://gateway.example/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));
  await waitFor(() => expect(onApply).toHaveBeenCalledWith(expect.objectContaining({
    provider: "anthropic-compatible",
    model: "custom-model",
    baseUrl: "https://gateway.example/v1",
  })));
});

it("keeps verification errors visible and retryable", async () => {
  const customProvider: ProviderCatalogEntry = {
    id: "custom",
    displayName: "Custom route",
    description: "Custom route",
    connection: "custom",
    profileProvider: "custom",
    authMethods: ["api-key"],
    defaultAuth: "api-key",
    defaultModel: "custom-model",
    modelOptions: ["custom-model"],
    browserOauth: false,
    discoverModels: false,
    customValues: false,
    currentCustom: false,
  };
  const customConfiguration = { ...configuration, provider: "custom", connection: "custom", configured: false, credentialConfigured: true };
  const onApply = vi.fn().mockRejectedValue(new Error("daemon unavailable"));
  render(<DesktopOnboarding configuration={customConfiguration} providers={[customProvider]} onApply={onApply} />);

  await waitFor(() => expect(screen.getByLabelText("Model permissions")).toHaveValue("ask-for-approval"));
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  fireEvent.click(screen.getByRole("button", { name: "Verify configuration" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("daemon unavailable");
  expect(screen.getByRole("button", { name: "Verify configuration" })).toBeEnabled();
});
