import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { startRuntime } from "./runtime";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
  return {
    ...actual,
    startRuntime: vi.fn(),
    closeRuntime: vi.fn(),
    pollRuntime: vi.fn().mockResolvedValue([]),
    submitRuntime: vi.fn(),
    runRuntimeCommand: vi.fn(),
    cancelRuntime: vi.fn(),
    configureRuntime: vi.fn(),
  };
});

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(startRuntime).mockReset();
});
afterEach(cleanup);

it("starts a general chat without requiring a project", async () => {
  vi.mocked(startRuntime).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  render(<App />);

  await waitFor(() => expect(startRuntime).toHaveBeenCalledWith(undefined));
  expect(screen.getByRole("heading", { name: "Medusa" })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "General chat" })).toBeInTheDocument();
  expect(screen.getByRole("textbox")).toBeEnabled();
  expect(screen.getByText("Medusa policy remains authoritative")).toBeInTheDocument();
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
