import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionModeControl } from "./PermissionModeControl";
import { loadPermissionModes } from "./permissionModes";

vi.mock("./permissionModes", () => ({
  loadPermissionModes: vi.fn(),
  setPermissionMode: vi.fn(),
}));

beforeEach(() => {
  const host = document.createElement("div");
  host.className = "composer-tools";
  document.body.appendChild(host);
});

afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

it("shows a loading state instead of claiming Full Access before permission state resolves", () => {
  vi.mocked(loadPermissionModes).mockReturnValue(new Promise(() => undefined));
  render(<PermissionModeControl />);

  expect(screen.getByRole("button", { name: "Loading permissions…" })).toBeInTheDocument();
  expect(screen.queryByText("Full Access")).not.toBeInTheDocument();
});

it("shows an unavailable state when permission loading fails", async () => {
  vi.mocked(loadPermissionModes).mockRejectedValue(new Error("permission store unavailable"));
  render(<PermissionModeControl />);

  expect(await screen.findByRole("button", { name: "Permissions unavailable" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Permissions unavailable" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("permission store unavailable");
});

it("shows the backend-selected permission mode once loaded", async () => {
  vi.mocked(loadPermissionModes).mockResolvedValue([
    {
      id: "ask-for-approval",
      label: "Ask for approval",
      description: "Ask before protected boundary actions.",
      active: true,
    },
    {
      id: "full-access",
      label: "Full Access",
      description: "Unrestricted access.",
      active: false,
    },
  ]);
  render(<PermissionModeControl />);

  await waitFor(() => expect(screen.getByRole("button", { name: "Ask for approval" })).toBeInTheDocument());
});
