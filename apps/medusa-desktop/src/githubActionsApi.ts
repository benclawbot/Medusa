import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

export interface GitHubMutationAuditRecord {
  operation: string;
  repository: string;
  runId: number;
  jobId: number;
  commitSha: string;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: string;
}

export interface GitHubActionsRetryResult {
  repository: string;
  runId: number;
  jobId: number;
  commitSha: string;
  audit: GitHubMutationAuditRecord;
}

export async function retryGitHubActionsJob(
  repository: string,
  runId: number,
  jobId: number,
  commitSha: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  hostname?: string,
): Promise<GitHubActionsRetryResult> {
  if (preview.kind !== "actionsRetry") {
    throw new Error("GitHub Actions retry requires an actionsRetry preview");
  }
  return invoke<GitHubActionsRetryResult>("runtime_retry_github_actions_job", {
    repository: repository.trim(),
    hostname: hostname?.trim() || null,
    runId,
    jobId,
    commitSha: commitSha.trim(),
    preview,
    confirmation,
  });
}
