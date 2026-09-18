import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { requestRuntimeResume, type SessionSummary } from "./runtime";
import { listRuntimeSessionPage } from "./sessionPaging";
import { formatSessionAge, SessionDock } from "./SessionDock";

vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
  return {
    ...actual,
    requestRuntimeResume: vi.fn(),
  };
});

vi.mock("./sessionPaging", async () => {
  const actual = await vi.importActual<typeof import("./sessionPaging")>("./sessionPaging");
  return {
    ...actual,
    listRuntimeSessionPage: vi.fn(),
  };
});

const sessions: SessionSummary[] = [
  {
    id: "session-alpha",
    objective: "Alpha session",
    createdAt: "2026-09-18T10:00:00Z",
    updatedAt: "2026-09-18T12:00:00Z",
    completed: true,
    waitingForUser: false,
    turn: 3,
  },
  {
    id: "session-beta",
    objective: "Beta session",
    createdAt: "2026-09-18T09:00:00Z",
    updatedAt: "2026-09-18T11:00:00Z",
    completed: true,
    waitingForUser: false,
    turn: 2,
  },
];

beforeEach(() => {
  vi.mocked(listRuntimeSessionPage).mockReset().mockResolvedValue({ sessions });
  vi.mocked(requestRuntimeResume).mockReset();
});

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it("renders the compact recent-session rail without the old Sessions navigation", () => {
  render(<SessionDock />);

  expect(screen.getByRole("region", { name: "Recent sessions" })).toBeInTheDocument();
  expect(screen.getByText("Recent")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Sessions" })).not.toBeInTheDocument();
});

it("formats recent-session age without redundant relative-time copy", () => {
  const now = Date.parse("2026-09-08T12:00:00Z");
  expect(formatSessionAge("2026-09-08T11:00:00Z", now)).toBe("1h");
  expect(formatSessionAge("2026-09-07T12:00:00Z", now)).toBe("1d");
});

it("offers rename, pin, archive, and delete without a share action", async () => {
  window.localStorage.setItem("medusa.desktop.repo", "/repo");
  render(<SessionDock />);

  const actions = await screen.findByRole("button", { name: "Actions for Alpha session" });
  fireEvent.click(actions);

  const menu = screen.getByRole("menu", { name: "Actions for Alpha session" });
  expect(within(menu).getByRole("menuitem", { name: "Rename" })).toBeInTheDocument();
  expect(within(menu).getByRole("menuitem", { name: "Pin chat" })).toBeInTheDocument();
  expect(within(menu).getByRole("menuitem", { name: "Archive" })).toBeInTheDocument();
  expect(within(menu).getByRole("menuitem", { name: "Delete" })).toBeInTheDocument();
  expect(within(menu).queryByText(/share/i)).not.toBeInTheDocument();

  fireEvent.click(within(menu).getByRole("menuitem", { name: "Pin chat" }));
  expect(await screen.findByLabelText("Pinned")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Actions for Alpha session" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
  const rename = screen.getByRole("textbox", { name: "Rename Alpha session" });
  fireEvent.change(rename, { target: { value: "Pinned project chat" } });
  fireEvent.keyDown(rename, { key: "Enter" });
  expect(await screen.findByText("Pinned project chat")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Pinned project chat" }));
  expect(requestRuntimeResume).toHaveBeenCalledWith("session-alpha", "/repo");
});

it("supports select all, individual untick, and bulk delete from Recent", async () => {
  window.localStorage.setItem("medusa.desktop.repo", "/repo");
  render(<SessionDock />);

  await screen.findByText("Alpha session");
  fireEvent.click(screen.getByRole("button", { name: "Recent session actions" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Select all" }));

  const alpha = screen.getByRole("checkbox", { name: "Select Alpha session" });
  const beta = screen.getByRole("checkbox", { name: "Select Beta session" });
  expect(alpha).toBeChecked();
  expect(beta).toBeChecked();

  fireEvent.click(beta);
  expect(alpha).toBeChecked();
  expect(beta).not.toBeChecked();

  fireEvent.click(screen.getByRole("button", { name: "Recent session actions" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete selected" }));

  await waitFor(() => expect(screen.queryByText("Alpha session")).not.toBeInTheDocument());
  expect(screen.getByText("Beta session")).toBeInTheDocument();
  expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
});

it("archives a session out of the Recent list", async () => {
  window.localStorage.setItem("medusa.desktop.repo", "/repo");
  render(<SessionDock />);

  const actions = await screen.findByRole("button", { name: "Actions for Beta session" });
  fireEvent.click(actions);
  fireEvent.click(screen.getByRole("menuitem", { name: "Archive" }));

  await waitFor(() => expect(screen.queryByText("Beta session")).not.toBeInTheDocument());
  expect(screen.getByText("Alpha session")).toBeInTheDocument();
});
