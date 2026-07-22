import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { createGitHubIssue, updateGitHubIssue } from "./githubIssueMutationsApi";
import type { GitHubMutationConfirmation, GitHubMutationPreview } from "./githubMutation";

const confirmation: GitHubMutationConfirmation = {
  previewFingerprint: "confirmed",
  confirmedAt: "2026-07-22T00:00:00Z",
};

function preview(
  kind: "issueCreate" | "issueUpdate",
  resource: string,
  destructive = false,
): GitHubMutationPreview {
  return {
    kind,
    repository: "octo/repo",
    branch: "main",
    title: "Issue mutation",
    body: "Details",
    recipients: [],
    affectedResources: [resource],
    destructive,
  };
}

describe("GitHub issue mutations", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes confirmed issue creation", async () => {
    invoke.mockResolvedValue({
      repository: "octo/repo",
      issueNumber: 42,
      title: "Bug",
      state: "open",
      url: "https://github.com/octo/repo/issues/42",
      audit: {},
    });

    await createGitHubIssue(
      " octo/repo ",
      " Bug ",
      " Details ",
      preview("issueCreate", "issue:new"),
      confirmation,
      " github.com ",
    );

    expect(invoke).toHaveBeenCalledWith("runtime_create_github_issue", {
      repository: "octo/repo",
      title: "Bug",
      body: "Details",
      hostname: "github.com",
      preview: preview("issueCreate", "issue:new"),
      confirmation,
    });
  });

  it("requires destructive confirmation only when closing", async () => {
    await expect(
      updateGitHubIssue(
        "octo/repo",
        42,
        { state: "closed" },
        preview("issueUpdate", "issue:42"),
        confirmation,
      ),
    ).rejects.toThrow("destructive preview");

    await expect(
      updateGitHubIssue(
        "octo/repo",
        42,
        { title: "Updated" },
        preview("issueUpdate", "issue:42", true),
        confirmation,
      ),
    ).rejects.toThrow("must not be marked destructive");

    expect(invoke).not.toHaveBeenCalled();
  });

  it("rejects empty and invalid updates before invoking Tauri", async () => {
    await expect(
      updateGitHubIssue(
        "octo/repo",
        0,
        { title: "Updated" },
        preview("issueUpdate", "issue:0"),
        confirmation,
      ),
    ).rejects.toThrow("positive integer");

    await expect(
      updateGitHubIssue(
        "octo/repo",
        42,
        {},
        preview("issueUpdate", "issue:42"),
        confirmation,
      ),
    ).rejects.toThrow("at least one changed field");

    expect(invoke).not.toHaveBeenCalled();
  });
});
