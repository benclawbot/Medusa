import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  getTimelineSnapshot,
  pollRuntime,
  startRuntime,
  subscribeTimeline,
  type RuntimeEvent,
} from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mockedInvoke = vi.mocked(invoke);

describe("runtime timeline store", () => {
  beforeEach(() => {
    window.localStorage.clear();
    mockedInvoke.mockReset();
  });

  it("reduces typed runtime events into plan, activity, and busy state", async () => {
    mockedInvoke
      .mockResolvedValueOnce({ runtimeId: "runtime-1", repo: "/repo" })
      .mockResolvedValueOnce([
        { type: "started" },
        { type: "plan", steps: [{ title: "Inspect", status: "inProgress" }] },
        {
          type: "activity",
          activity: { id: "tool-1", kind: "tool", title: "Read files", details: ["src/App.tsx"] },
        },
      ] satisfies RuntimeEvent[]);

    await startRuntime("/repo");
    await pollRuntime("runtime-1");

    expect(getTimelineSnapshot()).toEqual({
      runtimeId: "runtime-1",
      busy: true,
      plan: [{ title: "Inspect", status: "inProgress" }],
      activities: [{ id: "tool-1", kind: "tool", title: "Read files", details: ["src/App.tsx"] }],
    });
  });

  it("updates activities by stable id and clears busy on completion", async () => {
    mockedInvoke
      .mockResolvedValueOnce({ runtimeId: "runtime-2", repo: "/repo" })
      .mockResolvedValueOnce([
        { type: "started" },
        { type: "activity", activity: { id: "tool-1", kind: "tool", title: "Test", details: [] } },
      ] satisfies RuntimeEvent[])
      .mockResolvedValueOnce([
        { type: "activity", activity: { id: "tool-1", kind: "done", title: "Test", details: ["passed"] } },
        { type: "turnFinished" },
      ] satisfies RuntimeEvent[]);

    await startRuntime("/repo");
    await pollRuntime("runtime-2");
    await pollRuntime("runtime-2");

    expect(getTimelineSnapshot().activities).toEqual([
      { id: "tool-1", kind: "done", title: "Test", details: ["passed"] },
    ]);
    expect(getTimelineSnapshot().busy).toBe(false);
  });

  it("notifies subscribers and resets when a new runtime starts", async () => {
    const listener = vi.fn();
    const unsubscribe = subscribeTimeline(listener);
    mockedInvoke
      .mockResolvedValueOnce({ runtimeId: "runtime-a", repo: "/a" })
      .mockResolvedValueOnce({ runtimeId: "runtime-b", repo: "/b" });

    await startRuntime("/a");
    await startRuntime("/b");

    expect(listener).toHaveBeenCalledTimes(2);
    expect(getTimelineSnapshot()).toEqual({ runtimeId: "runtime-b", plan: [], activities: [], busy: false });
    unsubscribe();
  });
});
