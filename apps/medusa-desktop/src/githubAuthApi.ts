import { invoke } from "@tauri-apps/api/core";

export type GithubAuthState =
  | "ready"
  | "ghUnavailable"
  | "unauthenticated"
  | "invalidCredentials"
  | "missingScopes"
  | "unknownFailure";

export interface GithubAuthStatus {
  state: GithubAuthState;
  hostname: string;
  account: string | null;
  scopes: string[];
  missingScopes: string[];
  message: string;
}

export async function readGithubAuthStatus(
  hostname = "github.com",
): Promise<GithubAuthStatus> {
  const normalized = hostname.trim();
  if (!normalized) {
    throw new Error("GitHub hostname is required");
  }
  return invoke<GithubAuthStatus>("runtime_github_auth_status", {
    hostname: normalized,
  });
}
