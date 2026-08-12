import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { readGitHubIssues } from "./githubIssuesApi";

describe("readGitHubIssues", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes inputs and invokes the typed Tauri command", async () => {
    invoke.mockResolvedValue({ repository: "octo/repo", issues: [] });

    await readGitHubIssues(" octo/repo ", "closed", " github.com ");

    expect(invoke).toHaveBeenCalledWith("runtime_github_issues", {
      repository: "octo/repo",
      hostname: "github.com",
      state: "closed",
    });
  });

  it("uses open issues by default", async () => {
    invoke.mockResolvedValue({ repository: "octo/repo", issues: [] });

    await readGitHubIssues("octo/repo");

    expect(invoke).toHaveBeenCalledWith("runtime_github_issues", {
      repository: "octo/repo",
      hostname: null,
      state: "open",
    });
  });
});
