import { invoke } from "@tauri-apps/api/core";

export type GithubRepositoryAccessState =
  | "ready"
  | "ghUnavailable"
  | "authenticationRequired"
  | "notFound"
  | "forbidden"
  | "unknownFailure";

export interface GithubRepositoryAccess {
  state: GithubRepositoryAccessState;
  hostname: string;
  repository: string;
  visibility: string | null;
  defaultBranch: string | null;
  permissions: string[];
  message: string;
}

export function readGithubRepositoryAccess(
  repository: string,
  hostname?: string,
): Promise<GithubRepositoryAccess> {
  return invoke<GithubRepositoryAccess>("runtime_github_repository_access", {
    repository: repository.trim(),
    hostname: hostname?.trim() || null,
  });
}
