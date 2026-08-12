import { invoke } from "@tauri-apps/api/core";

export type GitHubIssueState = "open" | "closed" | "all";

export interface GitHubIssueSummary {
  number: number;
  title: string;
  state: string;
  author?: string;
  labels: string[];
  url?: string;
}

export interface GitHubIssueList {
  repository: string;
  issues: GitHubIssueSummary[];
}

export function readGitHubIssues(
  repository: string,
  state: GitHubIssueState = "open",
  hostname?: string,
): Promise<GitHubIssueList> {
  return invoke<GitHubIssueList>("runtime_github_issues", {
    repository: repository.trim(),
    hostname: hostname?.trim() || null,
    state,
  });
}
