import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

export type GitHubMergeMethod = "merge" | "squash" | "rebase";

export interface PullRequestMergeAudit {
  operation: "pullRequestMerge";
  repository: string;
  pullRequestNumber: number;
  expectedHeadSha: string;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: "merged";
}

export interface PullRequestMergeResult {
  repository: string;
  pullRequestNumber: number;
  expectedHeadSha: string;
  mergeCommitSha: string;
  mergeMethod: GitHubMergeMethod;
  audit: PullRequestMergeAudit;
}

export async function mergeGitHubPullRequest(
  repository: string,
  pullRequestNumber: number,
  expectedHeadSha: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  mergeMethod: GitHubMergeMethod = "squash",
  hostname?: string,
): Promise<PullRequestMergeResult> {
  if (preview.kind !== "pullRequestMerge") {
    throw new Error("Pull request merge requires a pullRequestMerge preview");
  }
  if (!preview.destructive) {
    throw new Error("Pull request merge preview must be marked destructive");
  }
  return invoke<PullRequestMergeResult>("runtime_merge_github_pull_request", {
    repository: repository.trim(),
    pullRequestNumber,
    expectedHeadSha: expectedHeadSha.trim(),
    mergeMethod,
    hostname: hostname?.trim() || null,
    preview,
    confirmation,
  });
}
