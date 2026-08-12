import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { mergeGitHubPullRequest } from "./githubMergeApi";
import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

const preview: GitHubMutationPreview = {
  kind: "pullRequestMerge",
  repository: "octo/repo",
  branch: "abcdef1",
  title: "Merge pull request #42",
  recipients: [],
  affectedResources: ["pull-request:42"],
  destructive: true,
};

const confirmation: GitHubMutationConfirmation = {
  previewFingerprint: "fingerprint",
  confirmedAt: "2026-07-22T00:00:00Z",
};

describe("mergeGitHubPullRequest", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes inputs and invokes the confirmed merge command", async () => {
    invoke.mockResolvedValue({
      repository: "octo/repo",
      pullRequestNumber: 42,
      expectedHeadSha: "abcdef1",
      mergeCommitSha: "1234567",
      mergeMethod: "squash",
      audit: {
        operation: "pullRequestMerge",
        repository: "octo/repo",
        pullRequestNumber: 42,
        expectedHeadSha: "abcdef1",
        previewFingerprint: "fingerprint",
        confirmedAt: "2026-07-22T00:00:00Z",
        outcome: "merged",
      },
    });

    await mergeGitHubPullRequest(
      " octo/repo ",
      42,
      " abcdef1 ",
      preview,
      confirmation,
      "squash",
      " github.com ",
    );

    expect(invoke).toHaveBeenCalledWith("runtime_merge_github_pull_request", {
      repository: "octo/repo",
      pullRequestNumber: 42,
      expectedHeadSha: "abcdef1",
      mergeMethod: "squash",
      hostname: "github.com",
      preview,
      confirmation,
    });
  });

  it("rejects a non-destructive merge preview before invoke", async () => {
    await expect(
      mergeGitHubPullRequest(
        "octo/repo",
        42,
        "abcdef1",
        { ...preview, destructive: false },
        confirmation,
      ),
    ).rejects.toThrow("marked destructive");
    expect(invoke).not.toHaveBeenCalled();
  });
});
