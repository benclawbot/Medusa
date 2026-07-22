import { invoke } from "@tauri-apps/api/core";

import type {
  GitHubMutationConfirmation,
  GitHubMutationPreview,
} from "./githubMutation";

export interface GitMutationResult {
  branch: string;
  commitSha: string;
  checkpointRef?: string;
}

export async function createBranch(
  repo: string,
  branch: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
): Promise<GitMutationResult> {
  return invoke<GitMutationResult>("runtime_create_branch", {
    repo,
    branch,
    preview,
    confirmation,
  });
}

export async function createCheckpoint(
  repo: string,
  checkpointRef: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
): Promise<GitMutationResult> {
  return invoke<GitMutationResult>("runtime_create_checkpoint", {
    repo,
    checkpointRef,
    preview,
    confirmation,
  });
}

export async function commitChanges(
  repo: string,
  message: string,
  paths: string[],
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
): Promise<GitMutationResult> {
  return invoke<GitMutationResult>("runtime_commit_changes", {
    repo,
    message,
    paths,
    preview,
    confirmation,
  });
}

export async function pushBranch(
  repo: string,
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation,
  remote?: string,
): Promise<GitMutationResult> {
  return invoke<GitMutationResult>("runtime_push_branch", {
    repo,
    remote,
    preview,
    confirmation,
  });
}
