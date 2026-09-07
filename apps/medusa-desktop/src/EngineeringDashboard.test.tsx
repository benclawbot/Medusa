import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DESKTOP_TOOL_EVENT } from "./desktop-tools";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { RUNTIME_DATA_CHANGED_EVENT } from "./runtime";

const { loadEngineeringDashboard } = vi.hoisted(() => ({
  loadEngineeringDashboard: vi.fn(),
}));

vi.mock("./engineeringApi", () => ({ loadEngineeringDashboard }));

const data = {
  totalTasks: 1,
  successfulTasks: 1,
  successRate: 100,
  verificationPassRate: 100,
  averageRetries: 0,
  humanInterventionRate: 0,
  rollbackRate: 0,
  averageDurationMinutes: 1.2,
  trend: [],
  friction: [],
  improvements: [],
  generatedAt: "2026-09-07T20:00:00Z",
};

beforeEach(() => {
  window.localStorage.clear();
  loadEngineeringDashboard.mockReset().mockResolvedValue(data);
});

afterEach(() => {
  cleanup();
});

it("reloads the open dashboard when a runtime turn completes", async () => {
  window.localStorage.setItem("medusa.desktop.repo", "C:/repo");
  render(<EngineeringDashboardLauncher />);
  window.dispatchEvent(new CustomEvent(DESKTOP_TOOL_EVENT, { detail: "engineering" }));

  await screen.findByRole("dialog", { name: "Engineering dashboard" });
  await waitFor(() => expect(loadEngineeringDashboard).toHaveBeenCalledWith("C:/repo", 90));
  const callsBeforeChange = loadEngineeringDashboard.mock.calls.length;

  window.dispatchEvent(new CustomEvent(RUNTIME_DATA_CHANGED_EVENT, { detail: "runtime-1" }));

  await waitFor(() => expect(loadEngineeringDashboard.mock.calls.length).toBe(callsBeforeChange + 1));
});
