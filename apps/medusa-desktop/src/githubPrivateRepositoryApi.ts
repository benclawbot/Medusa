import { invoke } from "@tauri-apps/api/core";

export type GitHubRepositoryTransferState =
  | "ready"
  | "ghUnavailable"
  | "authenticationRequired"
  | "notFound"
  | "forbidden"
  | "invalidTarget"
  | "dirtyWorktree"
  | "failed";

export interface GitHubRepositoryTransferResult {
  state: GitHubRepositoryTransferState;
  repository: string;
  path: string;
  operation: "clone" | "fetch";
  message: string;
}

export async function cloneGitHubRepository(
  repository: string,
  destination: string,
  hostname?: string,
): Promise<GitHubRepositoryTransferResult> {
  const normalizedRepository = repository.trim();
  const normalizedDestination = destination.trim();
  if (!normalizedRepository) throw new Error("GitHub repository is required");
  if (!normalizedDestination) throw new Error("Clone destination is required");
  return invoke<GitHubRepositoryTransferResult>("runtime_clone_github_repository", {
    repository: normalizedRepository,
    destination: normalizedDestination,
    hostname: hostname?.trim() || null,
  });
}

export async function fetchGitHubRepository(
  repository: string,
  localPath: string,
  hostname?: string,
): Promise<GitHubRepositoryTransferResult> {
  const normalizedRepository = repository.trim();
  const normalizedLocalPath = localPath.trim();
  if (!normalizedRepository) throw new Error("GitHub repository is required");
  if (!normalizedLocalPath) throw new Error("Local repository path is required");
  return invoke<GitHubRepositoryTransferResult>("runtime_fetch_github_repository", {
    repository: normalizedRepository,
    localPath: normalizedLocalPath,
    hostname: hostname?.trim() || null,
  });
}
