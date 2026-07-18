import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { pollRuntime, startRuntime, submitRuntime } from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);

describe("desktop runtime adapter", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("starts the shared runtime for a selected repository", async () => {
    mockedInvoke.mockResolvedValueOnce({ runtimeId: "runtime-1", repo: "/repo" });
    await expect(startRuntime("/repo")).resolves.toEqual({ runtimeId: "runtime-1", repo: "/repo" });
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_start", { repo: "/repo" });
  });

  it("submits prompts and polls typed events", async () => {
    mockedInvoke.mockResolvedValueOnce("queued");
    await expect(submitRuntime("runtime-1", { text: "more detail", attachments: [], revision: 2 })).resolves.toBe("queued");
    mockedInvoke.mockResolvedValueOnce([{ type: "progress", turn: 4 }]);
    await expect(pollRuntime("runtime-1")).resolves.toEqual([{ type: "progress", turn: 4 }]);
  });
});
