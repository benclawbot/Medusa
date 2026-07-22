import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { readGitHubCommitChecks } from "./githubChecksApi";

describe("readGitHubCommitChecks", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes inputs and invokes the typed Tauri command", async () => {
    invoke.mockResolvedValue({
      repository: "octo/repo",
      commitSha: "abcdef1",
      checks: [],
      message: "GitHub commit checks are ready",
    });

    await readGitHubCommitChecks(" octo/repo ", " abcdef1 ", " github.com ");

    expect(invoke).toHaveBeenCalledWith("runtime_github_commit_checks", {
      repository: "octo/repo",
      commitSha: "abcdef1",
      hostname: "github.com",
    });
  });
});
