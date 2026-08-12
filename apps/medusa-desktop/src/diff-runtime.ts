import { invoke } from "@tauri-apps/api/core";

export type DiffStatus = "added" | "deleted" | "modified" | "renamed";
export type DiffLineKind = "context" | "addition" | "deletion" | "meta";

export interface DiffLine {
  kind: DiffLineKind;
  oldLine?: number;
  newLine?: number;
  text: string;
}

export interface DiffHunk {
  header: string;
  lines: DiffLine[];
}

export interface DiffFile {
  oldPath: string;
  newPath: string;
  status: DiffStatus;
  binary: boolean;
  additions: number;
  deletions: number;
  hunks: DiffHunk[];
}

export interface RepositoryDiff {
  files: DiffFile[];
  additions: number;
  deletions: number;
}

export async function readRuntimeDiff(repo: string): Promise<RepositoryDiff> {
  return invoke<RepositoryDiff>("runtime_read_diff", { repo });
}
