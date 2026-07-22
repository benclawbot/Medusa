import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  readRuntimeSession,
  requestRuntimeResume,
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
  expect(window.localStorage.getItem("medusa.desktop.resumeSession")).toBe("session-456");
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
