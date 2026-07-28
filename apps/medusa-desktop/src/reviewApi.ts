import { invoke } from "@tauri-apps/api/core";

export type ChangeKind = "Added" | "Modified" | "Deleted" | "Renamed";
export type ChangeOrigin = "Medusa" | "PreExistingUser" | "Generated";
export type VerificationState = "Verified" | "Failed" | "Stale" | "Unverified";
export type ReviewState = "Unreviewed" | "Accepted" | "Reverted";

export interface ReviewProvenance {
  task_step_id?: string;
  tool_execution_id?: string;
  rationale?: string;
  verification_event_ids: string[];
}

export interface ReviewHunkModel {
  id: string;
  current_fingerprint: string;
  ambiguous: boolean;
  overlaps_later_edits: boolean;
  review_state: ReviewState;
  provenance: ReviewProvenance;
}

export interface ReviewFileModel {
  path: string;
  previous_path?: string;
  kind: ChangeKind;
  origin: ChangeOrigin;
  binary: boolean;
  policy_sensitive: boolean;
  verification: VerificationState;
  review_state: ReviewState;
  current_fingerprint: string;
  hunks: ReviewHunkModel[];
  provenance: ReviewProvenance;
}

export interface ReviewSnapshotModel {
  id: string;
  repository_fingerprint: string;
  created_at_unix_ms: number;
  files: ReviewFileModel[];
}

export interface ReviewDiffHunk {
  id: string;
  header: string;
  patch: string;
  ambiguous?: boolean;
  overlaps_later_edits?: boolean;
}

export interface ReviewDiffFile {
  path: string;
  previous_path?: string;
  patch: string;
  hunks: ReviewDiffHunk[];
}

export interface ReviewWorkspace {
  snapshot: ReviewSnapshotModel;
  files: ReviewDiffFile[];
  completion: {
    unreviewed_paths: string[];
    stale_or_failed_paths: string[];
    all_required_changes_reviewed: boolean;
    verification_current: boolean;
  };
  history?: unknown;
}

export const readReview = (repo: string) =>
  invoke<ReviewWorkspace>("runtime_read_review", { repo });

export const applyReviewAction = (
  repo: string,
  args: {
    operation: "accept-file" | "revert-file" | "revert-hunk" | "accept-task";
    path?: string;
    hunkId?: string;
    snapshotId: string;
    fileFingerprint?: string;
    hunkFingerprint?: string;
  },
) =>
  invoke<ReviewWorkspace>("runtime_apply_review_action", {
    repo,
    operation: args.operation,
    path: args.path,
    hunkId: args.hunkId,
    snapshotId: args.snapshotId,
    fileFingerprint: args.fileFingerprint,
    hunkFingerprint: args.hunkFingerprint,
  });

export const exportReviewAudit = (repo: string) =>
  invoke<Record<string, unknown>>("runtime_export_review_audit", { repo });
