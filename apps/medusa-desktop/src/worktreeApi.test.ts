import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import {
  hasBlockingWorktreeState,
  hasWorktreeChanges,
  readWorktreeStatus,
  type DesktopWorktreeStatus,
} from "./worktreeApi";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);

function cleanStatus(): DesktopWorktreeStatus {
  return {
    branch: "main",
    upstream: "origin/main",
    ahead: 0,
    behind: 0,
    detached: false,
    entries: [],
  };
}

describe("worktree status adapter", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("invokes the typed Tauri command for the selected repository", async () => {
    mockedInvoke.mockResolvedValueOnce(cleanStatus());

    await expect(readWorktreeStatus(" /repo ")).resolves.toEqual(cleanStatus());
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_read_worktree", { repo: "/repo" });
  });

  it("rejects an empty repository before invoking Tauri", async () => {
    await expect(readWorktreeStatus("   ")).rejects.toThrow(
      "A repository is required to inspect worktree status",
    );
    expect(mockedInvoke).not.toHaveBeenCalled();
  });

  it("blocks detached heads and conflicts while allowing ordinary dirty files", () => {
    const dirty = {
      ...cleanStatus(),
      entries: [
        {
          path: "src/main.rs",
          staged: "unmodified" as const,
          unstaged: "modified" as const,
          untracked: false,
          conflicted: false,
          ignored: false,
        },
      ],
    };

    expect(hasWorktreeChanges(dirty)).toBe(true);
    expect(hasBlockingWorktreeState(dirty)).toBe(false);
    expect(hasBlockingWorktreeState({ ...dirty, detached: true })).toBe(true);
    expect(
      hasBlockingWorktreeState({
        ...dirty,
        entries: [{ ...dirty.entries[0], conflicted: true }],
      }),
    ).toBe(true);
  });

  it("does not treat ignored-only entries as user changes", () => {
    expect(
      hasWorktreeChanges({
        ...cleanStatus(),
        entries: [
          {
            path: "target",
            staged: "unmodified",
            unstaged: "unmodified",
            untracked: false,
            conflicted: false,
            ignored: true,
          },
        ],
      }),
    ).toBe(false);
  });
});
