import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  readRuntimeSession,
  requestRuntimeResume,
  publishRepoChanged,
  resumeRuntime,
  REPO_CHANGED_EVENT,
  RUNTIME_RESUME_EVENT,
  startRuntime,
} from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  window.localStorage.clear();
  vi.mocked(invoke).mockReset();
});

it("consumes a pending repository session through runtime_resume", async () => {
  vi.mocked(invoke).mockResolvedValue({ runtimeId: "runtime-1", repo: "/repo" });
  requestRuntimeResume("session-123");

  await expect(startRuntime("/repo")).resolves.toEqual({ runtimeId: "runtime-1", repo: "/repo" });

  expect(invoke).toHaveBeenCalledWith("runtime_resume", {
    repo: "/repo",
    sessionId: "session-123",
  });
  expect(window.localStorage.getItem("medusa.desktop.resumeSession")).toBeNull();
});

it("keeps pending resume state until a repository is available", async () => {
  vi.mocked(invoke).mockResolvedValue({ runtimeId: "runtime-general", repo: "" });
  requestRuntimeResume("session-456");

  await startRuntime();

  expect(invoke).toHaveBeenCalledWith("runtime_start", {});
  expect(window.localStorage.getItem("medusa.desktop.resumeSession")).toBe(JSON.stringify({ sessionId: "session-456", repo: "" }));
});

it("notifies the active desktop when a session resume is requested", () => {
  const listener = vi.fn();
  window.addEventListener(RUNTIME_RESUME_EVENT, listener);

  requestRuntimeResume("session-789");

  expect(listener).toHaveBeenCalledTimes(1);
  expect((listener.mock.calls[0][0] as CustomEvent<{ sessionId: string; repo: string }>).detail).toEqual({
    sessionId: "session-789",
    repo: "",
  });
  window.removeEventListener(RUNTIME_RESUME_EVENT, listener);
});

it("drops a resume intent scoped to another repository instead of cross-wiring it", async () => {
  vi.mocked(invoke).mockResolvedValue({ runtimeId: "runtime-other", repo: "C:/other" });
  requestRuntimeResume("session-a", "C:/project-a");

  await expect(startRuntime("C:/project-b")).resolves.toEqual({ runtimeId: "runtime-other", repo: "C:/other" });

  expect(invoke).toHaveBeenCalledWith("runtime_start", { repo: "C:/project-b" });
  expect(invoke).not.toHaveBeenCalledWith("runtime_resume", expect.anything());
  expect(window.localStorage.getItem("medusa.desktop.resumeSession")).toBeNull();
});

it("clears a legacy pending intent after an explicit resume attempt", async () => {
  vi.mocked(invoke).mockResolvedValue({ runtimeId: "runtime-2", repo: "/repo" });
  requestRuntimeResume("session-explicit", "/repo");

  await expect(resumeRuntime("/repo", "session-explicit")).resolves.toEqual({
    runtimeId: "runtime-2",
    repo: "/repo",
  });

  expect(window.localStorage.getItem("medusa.desktop.resumeSession")).toBeNull();
});

it("publishes repository changes and keeps the shared repository pointer current", () => {
  const listener = vi.fn();
  window.addEventListener(REPO_CHANGED_EVENT, listener);

  publishRepoChanged("  C:/work/medusa  ");
  expect(window.localStorage.getItem("medusa.desktop.repo")).toBe("C:/work/medusa");
  expect((listener.mock.calls[0][0] as CustomEvent<string>).detail).toBe("C:/work/medusa");

  publishRepoChanged("");
  expect(window.localStorage.getItem("medusa.desktop.repo")).toBeNull();
  expect((listener.mock.calls[1][0] as CustomEvent<string>).detail).toBe("");
  window.removeEventListener(REPO_CHANGED_EVENT, listener);
});

it("reads a durable session transcript through the Tauri command", async () => {
  const detail = {
    summary: {
      id: "session-789",
      objective: "Repair release workflow",
      createdAt: "2026-07-22T05:00:00Z",
      updatedAt: "2026-07-22T05:30:00Z",
      completed: false,
      waitingForUser: true,
      turn: 4,
    },
    messages: [
      { role: "user", text: "Fix the workflow" },
      { role: "assistant", text: "I found the failing step." },
    ],
  };
  vi.mocked(invoke).mockResolvedValue(detail);

  await expect(readRuntimeSession("/repo", "session-789")).resolves.toEqual(detail);
  expect(invoke).toHaveBeenCalledWith("runtime_read_session", {
    repo: "/repo",
    sessionId: "session-789",
  });
});
