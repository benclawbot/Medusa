import { invoke } from "@tauri-apps/api/core";

export interface GitHubActionsJobLog {
  repository: string;
  jobId: number;
  content: string;
  truncated: boolean;
  redactedLines: number;
}

export async function readGitHubActionsJobLog(
  repository: string,
  jobId: number,
  hostname?: string,
): Promise<GitHubActionsJobLog> {
  const normalizedRepository = repository.trim();
  if (!normalizedRepository) {
    throw new Error("GitHub repository is required");
  }
  if (!Number.isSafeInteger(jobId) || jobId <= 0) {
    throw new Error("GitHub Actions job id must be a positive integer");
  }
  return invoke<GitHubActionsJobLog>("runtime_github_actions_job_log", {
    repository: normalizedRepository,
    hostname: hostname?.trim() || null,
    jobId,
  });
}
