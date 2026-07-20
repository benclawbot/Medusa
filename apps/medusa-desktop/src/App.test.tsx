import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { commandSuggestions, pollRuntime, runRuntimeCommand, startRuntime } from "./runtime";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
  return {
    ...actual,
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
  vi.mocked(startRuntime).mockReset();
  vi.mocked(commandSuggestions).mockReset().mockResolvedValue([]);
  vi.mocked(runRuntimeCommand).mockReset();
  vi.mocked(pollRuntime).mockReset().mockResolvedValue([]);
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
