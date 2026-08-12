import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import {
  configureRuntime,
  loadSharedConfiguration,
  startRuntime,
} from "./runtime";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
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

beforeEach(() => {
  window.localStorage.clear();
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
    runtimeId: "desktop-minimax",
    repo: "",
  });
  vi.mocked(configureRuntime).mockReset().mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("boots the desktop runtime from the saved MiniMax profile", async () => {
  render(<App />);

  await waitFor(() =>
    expect(configureRuntime).toHaveBeenCalledWith("desktop-minimax", {
      provider: "minimax",
      model: "MiniMax-M3",
      effort: "medium",
      expectedRevision: 7,
    }),
  );

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  expect(screen.getByLabelText("Provider")).toHaveValue("minimax");
  expect(screen.getByLabelText("Model")).toHaveValue("MiniMax-M3");
  expect(screen.getByText(/Shared profile: default · revision 7/)).toBeInTheDocument();
});
