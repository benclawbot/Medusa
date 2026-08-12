import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";
import { persistGitHubMutationAudit } from "./githubMutationAuditApi";

export type GitHubIssueState = "open" | "closed";

export interface GitHubIssueMutationAudit {
  operation: "issueCreate" | "issueUpdate";
  repository: string;
  issueNumber: number;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: "created" | "updated";
}

export interface GitHubIssueMutationResult {
  repository: string;
  issueNumber: number;
  title: string;
  state: GitHubIssueState;
  url: string;
  audit: GitHubIssueMutationAudit;
}

async function persistResultAudit(result: GitHubIssueMutationResult): Promise<void> {
  await persistGitHubMutationAudit({
    operation: result.audit.operation,
    repository: result.audit.repository,
    resource: `issue:${result.audit.issueNumber}`,
    previewFingerprint: result.audit.previewFingerprint,
    confirmedAt: result.audit.confirmedAt,
    outcome: result.audit.outcome,
  });
}

export async function createGitHubIssue(
  repository: string,
  title: string,
  body: string | undefined,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  hostname?: string,
): Promise<GitHubIssueMutationResult> {
  const normalizedRepository = repository.trim();
  const normalizedTitle = title.trim();
  const normalizedBody = body?.trim() || "";
  if (!normalizedRepository) throw new Error("GitHub repository is required");
  if (!normalizedTitle) throw new Error("GitHub issue title is required");
  if (preview.kind !== "issueCreate") {
    throw new Error("GitHub issue creation requires an issueCreate preview");
  }
  if (preview.destructive) {
    throw new Error("GitHub issue creation must not be marked destructive");
  }
  if (
    preview.mutationTitle?.trim() !== normalizedTitle ||
    preview.mutationBody?.trim() !== normalizedBody ||
    preview.mutationState !== undefined
  ) {
    throw new Error("GitHub issue preview content must match the requested mutation");
  }
  const result = await invoke<GitHubIssueMutationResult>("runtime_create_github_issue", {
    repository: normalizedRepository,
    title: normalizedTitle,
    body: normalizedBody || null,
    hostname: hostname?.trim() || null,
    preview,
    confirmation,
  });
  await persistResultAudit(result);
  return result;
}

export async function updateGitHubIssue(
  repository: string,
  issueNumber: number,
  changes: { title?: string; body?: string; state?: GitHubIssueState },
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  hostname?: string,
): Promise<GitHubIssueMutationResult> {
  const normalizedRepository = repository.trim();
  if (!normalizedRepository) throw new Error("GitHub repository is required");
  if (!Number.isSafeInteger(issueNumber) || issueNumber <= 0) {
    throw new Error("GitHub issue number must be a positive integer");
  }
  const title = changes.title?.trim();
  const body = changes.body?.trim();
  if (title === "") throw new Error("GitHub issue title cannot be blank");
  if (title === undefined && body === undefined && changes.state === undefined) {
    throw new Error("GitHub issue update requires at least one changed field");
  }
  if (preview.kind !== "issueUpdate") {
    throw new Error("GitHub issue update requires an issueUpdate preview");
  }
  if (changes.state === "closed" && !preview.destructive) {
    throw new Error("Closing a GitHub issue requires a destructive preview");
  }
  if (changes.state !== "closed" && preview.destructive) {
    throw new Error("Non-closing GitHub issue updates must not be marked destructive");
  }
  if (
    preview.mutationTitle?.trim() !== title ||
    preview.mutationBody?.trim() !== body ||
    preview.mutationState?.trim() !== changes.state
  ) {
    throw new Error("GitHub issue preview content must match the requested mutation");
  }
  const result = await invoke<GitHubIssueMutationResult>("runtime_update_github_issue", {
    repository: normalizedRepository,
    issueNumber,
    title: title ?? null,
    body: body ?? null,
    state: changes.state ?? null,
    hostname: hostname?.trim() || null,
    preview,
    confirmation,
  });
  await persistResultAudit(result);
  return result;
}
