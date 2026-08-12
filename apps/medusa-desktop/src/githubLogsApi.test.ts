import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { readGitHubActionsJobLog } from "./githubLogsApi";

describe("readGitHubActionsJobLog", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes the repository and hostname before invoking Tauri", async () => {
    invoke.mockResolvedValue({
      repository: "octo/repo",
      jobId: 42,
      content: "safe log\n",
      truncated: false,
      redactedLines: 0,
    });

    await readGitHubActionsJobLog(" octo/repo ", 42, " github.com ");

    expect(invoke).toHaveBeenCalledWith("runtime_github_actions_job_log", {
      repository: "octo/repo",
      hostname: "github.com",
      jobId: 42,
    });
  });

  it("rejects invalid targets before invoking Tauri", async () => {
    await expect(readGitHubActionsJobLog(" ", 42)).rejects.toThrow(
      "GitHub repository is required",
    );
    await expect(readGitHubActionsJobLog("octo/repo", 0)).rejects.toThrow(
      "positive integer",
    );
    expect(invoke).not.toHaveBeenCalled();
  });
});
