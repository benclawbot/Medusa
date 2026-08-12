import { invoke } from "@tauri-apps/api/core";

export type DesktopWorktreeChange =
  | "unmodified"
  | "added"
  | "modified"
  | "deleted"
  | "renamed"
  | "copied"
  | "unmerged"
  | "unknown";

export interface DesktopWorktreeEntry {
  path: string;
  originalPath?: string;
  staged: DesktopWorktreeChange;
  unstaged: DesktopWorktreeChange;
  untracked: boolean;
  conflicted: boolean;
  ignored: boolean;
}

export interface DesktopWorktreeStatus {
  branch?: string;
  upstream?: string;
  ahead: number;
  behind: number;
  detached: boolean;
  entries: DesktopWorktreeEntry[];
}

export async function readWorktreeStatus(repo: string): Promise<DesktopWorktreeStatus> {
  const normalizedRepo = repo.trim();
  if (!normalizedRepo) {
    throw new Error("A repository is required to inspect worktree status.");
  }
  return invoke<DesktopWorktreeStatus>("runtime_read_worktree", { repo: normalizedRepo });
}

export function hasBlockingWorktreeState(status: DesktopWorktreeStatus): boolean {
  return status.detached || status.entries.some((entry) => entry.conflicted);
}

export function hasWorktreeChanges(status: DesktopWorktreeStatus): boolean {
  return status.entries.some((entry) => !entry.ignored);
}
