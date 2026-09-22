import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { formatSessionAge, SessionDock } from "./SessionDock";
import {
  archiveRuntimeSession,
  deleteRuntimeSessions,
  listRuntimeSessionPage,
  renameRuntimeSession,
  setRuntimeSessionPinned,
} from "./sessionPaging";

vi.mock("./sessionPaging", () => ({
  listRuntimeSessionPage: vi.fn(),
  renameRuntimeSession: vi.fn(),
  setRuntimeSessionPinned: vi.fn(),
  archiveRuntimeSession: vi.fn(),
  deleteRuntimeSessions: vi.fn(),
}));

const sessions = [
  {
    id: "session-a",
    objective: "Alpha chat",
    createdAt: "2026-09-08T10:00:00Z",
    updatedAt: "2026-09-08T11:00:00Z",
    completed: true,
    waitingForUser: false,
    turn: 2,
    pinned: false,
  },
  {
    id: "session-b",
    objective: "Beta chat",
    createdAt: "2026-09-08T09:00:00Z",
    updatedAt: "2026-09-08T10:00:00Z",
    completed: true,
    waitingForUser: false,
    turn: 1,
    pinned: true,
  },
];

beforeEach(() => {
  window.localStorage.setItem("medusa.desktop.repo", "/repo");
  vi.mocked(listRuntimeSessionPage).mockReset().mockResolvedValue({ sessions });
  vi.mocked(renameRuntimeSession).mockReset().mockResolvedValue(undefined);
  vi.mocked(setRuntimeSessionPinned).mockReset().mockResolvedValue(undefined);
  vi.mocked(archiveRuntimeSession).mockReset().mockResolvedValue(undefined);
  vi.mocked(deleteRuntimeSessions).mockReset().mockResolvedValue(undefined);
  vi.spyOn(window, "confirm").mockReturnValue(true);
});

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  vi.restoreAllMocks();
});

it("renders the compact recent-session rail without the old Sessions navigation", async () => {
  render(<SessionDock />);

  expect(screen.getByRole("region", { name: "Recent sessions" })).toBeInTheDocument();
  expect(screen.getByText("Recent")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Sessions" })).not.toBeInTheDocument();
  expect(await screen.findByText("Alpha chat")).toBeInTheDocument();
});

it("formats recent-session age without redundant relative-time copy", () => {
  const now = Date.parse("2026-09-08T12:00:00Z");
  expect(formatSessionAge("2026-09-08T11:00:00Z", now)).toBe("1h");
  expect(formatSessionAge("2026-09-07T12:00:00Z", now)).toBe("1d");
});

it("opens a per-session menu with rename pin archive and delete but no share action", async () => {
  render(<SessionDock />);
  await screen.findByText("Alpha chat");

  fireEvent.click(screen.getByRole("button", { name: "Actions for Alpha chat" }));
  const menu = screen.getByRole("menu", { name: "Actions for Alpha chat" });

  expect(screen.getByRole("menuitem", { name: "Rename" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Pin chat" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Archive" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Delete" })).toBeInTheDocument();
  expect(menu).not.toHaveTextContent("Share");

  fireEvent.click(screen.getByRole("menuitem", { name: "Pin chat" }));
  await waitFor(() => expect(setRuntimeSessionPinned).toHaveBeenCalledWith("/repo", "session-a", true));
});

it("header menu can select all, individually untick, and bulk delete the remainder", async () => {
  render(<SessionDock />);
  await screen.findByText("Alpha chat");

  fireEvent.click(screen.getByRole("button", { name: "Recent session options" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Select all visible sessions" }));

  const alpha = screen.getByRole("checkbox", { name: "Select Alpha chat" });
  const beta = screen.getByRole("checkbox", { name: "Select Beta chat" });
  expect(alpha).toBeChecked();
  expect(beta).toBeChecked();

  fireEvent.click(beta);
  expect(beta).not.toBeChecked();

  fireEvent.click(screen.getByRole("button", { name: "Delete selected (1)" }));
  await waitFor(() => expect(deleteRuntimeSessions).toHaveBeenCalledWith("/repo", ["session-a"]));
});

it("header Select none enters selection mode with every recent session unticked", async () => {
  render(<SessionDock />);
  await screen.findByText("Alpha chat");

  fireEvent.click(screen.getByRole("button", { name: "Recent session options" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Select none" }));

  expect(screen.getByRole("checkbox", { name: "Select Alpha chat" })).not.toBeChecked();
  expect(screen.getByRole("checkbox", { name: "Select Beta chat" })).not.toBeChecked();
  expect(screen.getByRole("button", { name: "Delete selected" })).toBeDisabled();
});

it("can restore archived sessions", async () => {
  render(<SessionDock />);
  await screen.findByText("Alpha chat");

  fireEvent.click(screen.getByRole("button", { name: "Recent session options" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Show archived sessions" }));
  await waitFor(() => expect(listRuntimeSessionPage).toHaveBeenLastCalledWith("/repo", undefined, 24, true));

  fireEvent.click(screen.getByRole("button", { name: "Actions for Alpha chat" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Restore to Recent" }));
  await waitFor(() => expect(archiveRuntimeSession).toHaveBeenCalledWith("/repo", "session-a", false));
});

it("loads older sessions from the returned cursor", async () => {
  vi.mocked(listRuntimeSessionPage)
    .mockReset()
    .mockResolvedValueOnce({ sessions: [sessions[0]], nextCursor: "older" })
    .mockResolvedValueOnce({ sessions: [sessions[1]] });
  render(<SessionDock />);

  expect(await screen.findByText("Alpha chat")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Load older sessions" }));

  expect(await screen.findByText("Beta chat")).toBeInTheDocument();
  expect(listRuntimeSessionPage).toHaveBeenLastCalledWith("/repo", "older", 24, false);
});
