import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  getRuntimeWakeupMetrics,
  pollRuntime,
  runtimeWakeupPolicy,
  startRuntime,
  submitRuntime,
} from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);
const mockedListen = vi.mocked(listen);
let wakeListener: ((event: { payload: string }) => void) | undefined;

describe("desktop runtime adapter", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
    mockedListen.mockReset();
    wakeListener = undefined;
    mockedListen.mockImplementation(((_event: string, listener: (event: { payload: string }) => void) => {
      wakeListener = listener;
      // Keep installation pending so these adapter tests exercise polling without introducing a
      // second mocked IPC call for runtime_begin_wakeups.
      return new Promise(() => undefined);
    }) as typeof listen);
  });

  it("starts the shared runtime for a selected repository", async () => {
    mockedInvoke.mockResolvedValueOnce({ runtimeId: "runtime-1", repo: "/repo" });
    await expect(startRuntime("/repo")).resolves.toEqual({ runtimeId: "runtime-1", repo: "/repo" });
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_start", { repo: "/repo" });
  });

  it("starts the shared runtime without a repository", async () => {
    mockedInvoke.mockResolvedValueOnce({ runtimeId: "runtime-general", repo: "" });
    await expect(startRuntime()).resolves.toEqual({ runtimeId: "runtime-general", repo: "" });
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_start", {});
  });

  it("submits prompts and polls typed events", async () => {
    mockedInvoke.mockResolvedValueOnce("queued");
    await expect(submitRuntime("runtime-1", { text: "more detail", attachments: [], revision: 2 })).resolves.toBe("queued");
    mockedInvoke.mockResolvedValueOnce([{ type: "progress", turn: 4 }]);
    await expect(pollRuntime("runtime-1")).resolves.toEqual([{ type: "progress", turn: 4 }]);
  });

  it("skips redundant drains and performs a bounded fallback drain", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    mockedInvoke.mockResolvedValueOnce([]);

    await expect(pollRuntime("runtime-budget")).resolves.toEqual([]);
    await expect(pollRuntime("runtime-budget")).resolves.toEqual([]);

    expect(mockedInvoke).toHaveBeenCalledTimes(1);
    expect(getRuntimeWakeupMetrics("runtime-budget")).toMatchObject({
      drains: 1,
      localSkips: 1,
      fallbackDrains: 0,
      drainedEvents: 0,
    });

    now.mockReturnValue(1_000 + runtimeWakeupPolicy.fallbackPollMs + 1);
    mockedInvoke.mockResolvedValueOnce([]);
    await expect(pollRuntime("runtime-budget")).resolves.toEqual([]);

    expect(mockedInvoke).toHaveBeenCalledTimes(2);
    expect(getRuntimeWakeupMetrics("runtime-budget")).toMatchObject({
      drains: 2,
      localSkips: 1,
      fallbackDrains: 1,
    });
    now.mockRestore();
  });

  it("drains immediately after a native replay wakeup", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(5_000);
    mockedInvoke.mockResolvedValueOnce([]);
    await expect(pollRuntime("runtime-native-wake")).resolves.toEqual([]);

    expect(wakeListener).toBeDefined();
    wakeListener?.({ payload: "runtime-native-wake" });
    mockedInvoke.mockResolvedValueOnce([{ type: "progress", turn: 2 }]);
    await expect(pollRuntime("runtime-native-wake")).resolves.toEqual([{ type: "progress", turn: 2 }]);

    expect(getRuntimeWakeupMetrics("runtime-native-wake")).toMatchObject({
      nativeWakeups: 1,
      drains: 2,
      localSkips: 0,
      drainedEvents: 1,
    });
    now.mockRestore();
  });
});
