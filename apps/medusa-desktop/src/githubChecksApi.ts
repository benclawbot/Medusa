import { invoke } from "@tauri-apps/api/core";

export interface GitHubCheck {
  name: string;
  status: string;
  conclusion?: string;
  detailsUrl?: string;
}

export interface GitHubCommitChecks {
  repository: string;
  commitSha: string;
  checks: GitHubCheck[];
  message: string;
}

export function readGitHubCommitChecks(
  repository: string,
  commitSha: string,
  hostname?: string,
): Promise<GitHubCommitChecks> {
  return invoke<GitHubCommitChecks>("runtime_github_commit_checks", {
    repository: repository.trim(),
    commitSha: commitSha.trim(),
    hostname: hostname?.trim() || null,
  });
}
