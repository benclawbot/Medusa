import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  reviewGitHubPullRequest,
  updateGitHubPullRequest,
} from "./githubPullRequestMutationsApi";
import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

const confirmation: GitHubMutationConfirmation = {
  previewFingerprint: "confirmed",
  confirmedAt: "2026-07-22T00:00:00Z",
};

function preview(
  kind: "pullRequestUpdate" | "pullRequestReview",
  destructive: boolean,
  mutation: { title?: string; body?: string; state?: string },
): GitHubMutationPreview {
  return {
    kind,
    repository: "octo/repo",
    branch: "feature",
    title: "Pull request mutation",
    body: "Details",
    recipients: [],
    affectedResources: ["pullRequest:42"],
    destructive,
    mutationTitle: mutation.title,
    mutationBody: mutation.body,
    mutationState: mutation.state,
  };
}

describe("GitHub pull request mutations", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes a confirmed pull request update and persists its audit", async () => {
    const audit = {
      operation: "pullRequestUpdate" as const,
      repository: "octo/repo",
      pullRequestNumber: 42,
      previewFingerprint: "confirmed",
      confirmedAt: "2026-07-22T00:00:00Z",
      outcome: "updated" as const,
    };
    invoke
      .mockResolvedValueOnce({
        repository: "octo/repo",
        pullRequestNumber: 42,
        title: "Updated",
        state: "open",
        url: "https://github.com/octo/repo/pull/42",
        audit,
      })
      .mockResolvedValueOnce({ persisted: true, receiptPath: "/tmp/audit.jsonl" });

    const mutationPreview = preview("pullRequestUpdate", false, {
      title: "Updated",
      body: "Body",
      state: "state=open;base=main",
    });
    await updateGitHubPullRequest(
      " octo/repo ",
      42,
      { title: " Updated ", body: " Body ", state: "open", base: " main " },
      mutationPreview,
      confirmation,
      " github.com ",
    );

    expect(invoke).toHaveBeenNthCalledWith(1, "runtime_update_github_pull_request", {
      repository: "octo/repo",
      pullRequestNumber: 42,
      title: "Updated",
      body: "Body",
      state: "open",
      base: "main",
      hostname: "github.com",
      preview: mutationPreview,
      confirmation,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "runtime_persist_github_mutation_audit", {
      receipt: {
        operation: "pullRequestUpdate",
        repository: "octo/repo",
        resource: "pullRequest:42",
        previewFingerprint: "confirmed",
        confirmedAt: "2026-07-22T00:00:00Z",
        outcome: "updated",
      },
    });
  });

  it("requires destructive confirmation for closing", async () => {
    await expect(
      updateGitHubPullRequest(
        "octo/repo",
        42,
        { state: "closed" },
        preview("pullRequestUpdate", false, {
          state: "state=closed;base=",
        }),
        confirmation,
      ),
    ).rejects.toThrow("destructive flag");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("rejects update payload substitution", async () => {
    await expect(
      updateGitHubPullRequest(
        "octo/repo",
        42,
        { title: "Tampered" },
        preview("pullRequestUpdate", false, { title: "Safe" }),
        confirmation,
      ),
    ).rejects.toThrow("preview content must match");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("normalizes a commit-bound review and persists its audit", async () => {
    const commitId = "a".repeat(40);
    const audit = {
      operation: "pullRequestReview" as const,
      repository: "octo/repo",
      pullRequestNumber: 42,
      previewFingerprint: "confirmed",
      confirmedAt: "2026-07-22T00:00:00Z",
      outcome: "submitted" as const,
    };
    invoke
      .mockResolvedValueOnce({
        repository: "octo/repo",
        pullRequestNumber: 42,
        state: "APPROVED",
        url: "https://github.com/octo/repo/pull/42#pullrequestreview-1",
        reviewId: 1,
        audit,
      })
      .mockResolvedValueOnce({ persisted: true, receiptPath: "/tmp/audit.jsonl" });

    const mutationPreview = preview("pullRequestReview", false, {
      body: "Looks good",
      state: `action=approve;commit=${commitId}`,
    });
    await reviewGitHubPullRequest(
      "octo/repo",
      42,
      "approve",
      " Looks good ",
      commitId,
      mutationPreview,
      confirmation,
    );

    expect(invoke).toHaveBeenNthCalledWith(1, "runtime_review_github_pull_request", {
      repository: "octo/repo",
      pullRequestNumber: 42,
      action: "approve",
      body: "Looks good",
      commitId,
      hostname: null,
      preview: mutationPreview,
      confirmation,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, "runtime_persist_github_mutation_audit", {
      receipt: {
        operation: "pullRequestReview",
        repository: "octo/repo",
        resource: "pullRequest:42",
        previewFingerprint: "confirmed",
        confirmedAt: "2026-07-22T00:00:00Z",
        outcome: "submitted",
      },
    });
  });

  it("rejects unsafe review requests before invoking Tauri", async () => {
    await expect(
      reviewGitHubPullRequest(
        "octo/repo",
        42,
        "request_changes",
        undefined,
        undefined,
        preview("pullRequestReview", false, {
          state: "action=request_changes;commit=",
        }),
        confirmation,
      ),
    ).rejects.toThrow("body is required");

    await expect(
      reviewGitHubPullRequest(
        "octo/repo",
        42,
        "approve",
        undefined,
        "short",
        preview("pullRequestReview", false, {
          state: "action=approve;commit=short",
        }),
        confirmation,
      ),
    ).rejects.toThrow("full commit SHA");

    expect(invoke).not.toHaveBeenCalled();
  });
});
