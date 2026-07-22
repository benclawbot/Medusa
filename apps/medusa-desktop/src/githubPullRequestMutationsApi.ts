import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

export type GitHubPullRequestState = "open" | "closed";
export type GitHubPullRequestReviewAction = "approve" | "comment" | "request_changes";

export interface GitHubPullRequestMutationAudit {
  operation: "pullRequestUpdate" | "pullRequestReview";
  repository: string;
  pullRequestNumber: number;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: "updated" | "submitted";
}

export interface GitHubPullRequestMutationResult {
  repository: string;
  pullRequestNumber: number;
  title?: string;
  state: string;
  url: string;
  reviewId?: number;
  audit: GitHubPullRequestMutationAudit;
}

function requireTarget(repository: string, pullRequestNumber: number): string {
  const normalizedRepository = repository.trim();
  if (!normalizedRepository) throw new Error("GitHub repository is required");
  if (!Number.isSafeInteger(pullRequestNumber) || pullRequestNumber <= 0) {
    throw new Error("GitHub pull request number must be a positive integer");
  }
  return normalizedRepository;
}

function encodedUpdateState(state?: GitHubPullRequestState, base?: string): string | undefined {
  if (state === undefined && base === undefined) return undefined;
  return `state=${state ?? ""};base=${base?.trim() ?? ""}`;
}

export async function updateGitHubPullRequest(
  repository: string,
  pullRequestNumber: number,
  changes: {
    title?: string;
    body?: string;
    state?: GitHubPullRequestState;
    base?: string;
  },
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  hostname?: string,
): Promise<GitHubPullRequestMutationResult> {
  const normalizedRepository = requireTarget(repository, pullRequestNumber);
  const title = changes.title?.trim();
  const body = changes.body?.trim();
  const base = changes.base?.trim();
  if (title === "") throw new Error("GitHub pull request title cannot be blank");
  if (base === "") throw new Error("GitHub pull request base cannot be blank");
  if (title === undefined && body === undefined && changes.state === undefined && base === undefined) {
    throw new Error("GitHub pull request update requires at least one changed field");
  }
  if (preview.kind !== "pullRequestUpdate") {
    throw new Error("GitHub pull request update requires a pullRequestUpdate preview");
  }
  const destructive = changes.state === "closed";
  if (preview.destructive !== destructive) {
    throw new Error("GitHub pull request preview destructive flag does not match the update");
  }
  if (
    preview.mutationTitle?.trim() !== title ||
    preview.mutationBody?.trim() !== body ||
    preview.mutationState?.trim() !== encodedUpdateState(changes.state, base)
  ) {
    throw new Error("GitHub pull request preview content must match the requested update");
  }

  return invoke<GitHubPullRequestMutationResult>("runtime_update_github_pull_request", {
    repository: normalizedRepository,
    pullRequestNumber,
    title: title ?? null,
    body: body ?? null,
    state: changes.state ?? null,
    base: base ?? null,
    hostname: hostname?.trim() || null,
    preview,
    confirmation,
  });
}

export async function reviewGitHubPullRequest(
  repository: string,
  pullRequestNumber: number,
  action: GitHubPullRequestReviewAction,
  body: string | undefined,
  commitId: string | undefined,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  hostname?: string,
): Promise<GitHubPullRequestMutationResult> {
  const normalizedRepository = requireTarget(repository, pullRequestNumber);
  const normalizedBody = body?.trim();
  const normalizedCommitId = commitId?.trim();
  if ((action === "comment" || action === "request_changes") && !normalizedBody) {
    throw new Error("GitHub pull request review body is required for this action");
  }
  if (normalizedCommitId !== undefined && !/^[0-9a-fA-F]{40}$/.test(normalizedCommitId)) {
    throw new Error("GitHub review commit id must be a full commit SHA");
  }
  if (preview.kind !== "pullRequestReview") {
    throw new Error("GitHub pull request review requires a pullRequestReview preview");
  }
  if (preview.destructive) {
    throw new Error("GitHub pull request review must not be marked destructive");
  }
  const encodedState = `action=${action};commit=${normalizedCommitId ?? ""}`;
  if (
    preview.mutationTitle !== undefined ||
    preview.mutationBody?.trim() !== normalizedBody ||
    preview.mutationState?.trim() !== encodedState
  ) {
    throw new Error("GitHub pull request preview content must match the requested review");
  }

  return invoke<GitHubPullRequestMutationResult>("runtime_review_github_pull_request", {
    repository: normalizedRepository,
    pullRequestNumber,
    action,
    body: normalizedBody ?? null,
    commitId: normalizedCommitId ?? null,
    hostname: hostname?.trim() || null,
    preview,
    confirmation,
  });
}
