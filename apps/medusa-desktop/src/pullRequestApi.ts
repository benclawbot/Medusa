import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

export interface DraftPullRequestResult {
  branch: string;
  commitSha: string;
  pullRequestUrl: string;
}

export async function createDraftPullRequest(
  repo: string,
  base: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
): Promise<DraftPullRequestResult> {
  if (preview.kind !== "pullRequest") {
    throw new Error("Draft pull request creation requires a pullRequest preview");
  }
  return invoke<DraftPullRequestResult>("runtime_create_draft_pull_request", {
    repo,
    base,
    preview,
    confirmation,
  });
}
