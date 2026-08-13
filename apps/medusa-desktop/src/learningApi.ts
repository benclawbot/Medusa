import { invoke } from "@tauri-apps/api/core";

export type LearningReviewState =
  | "proposed"
  | "deferred"
  | "approved"
  | "rejected"
  | "validated"
  | "active"
  | "suspended"
  | "rolled_back"
  | "deleted"
  | "conflict";

export interface LearningPrivacy {
  captureEnabled: boolean;
  userPersistenceEnabled: boolean;
  crossRepositoryReuseEnabled: boolean;
  telemetryEnabled: boolean;
  automaticProposalsEnabled: boolean;
}

export interface LearningReviewItem {
  id: string;
  revision: number;
  state: LearningReviewState;
  kind: string;
  title: string;
  sourceSignalIds: string[];
  evidenceDigests: string[];
  rootCause: string;
  generalizedRule: string;
  scope: string;
  confidenceMilli: number;
  proposedSolution: string;
  nonApplicableContexts: string[];
  replay?: {
    reproduced: boolean;
    resolved: boolean;
    regressionCount: number;
    evidenceDigests: string[];
  };
  conflictsWith: string[];
  activeVersion?: string;
  previousVersion?: string;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export interface LearningReviewSnapshot {
  schemaVersion: number;
  revision: number;
  privacy: LearningPrivacy;
  items: LearningReviewItem[];
  auditHead: string;
}

export interface RedactionPreview {
  safe: boolean;
  blockedFields: string[];
  warnings: string[];
  itemCount: number;
}

export function loadLearningReview(repo: string) {
  return invoke<LearningReviewSnapshot>("runtime_learning_review", { repo });
}

export function transitionLearning(
  repo: string,
  id: string,
  action: "approve" | "reject" | "defer" | "validate" | "activate" | "suspend" | "rollback" | "delete",
  expectedRevision: number,
) {
  return invoke<LearningReviewSnapshot>("runtime_learning_transition", {
    repo,
    id,
    action,
    expectedRevision,
  });
}

export function inspectLearning(repo: string, id: string) {
  return invoke<string[]>("runtime_learning_inspect", { repo, id });
}

export function proposeLearning(
  repo: string,
  scope: "repository" | "user" | "session",
  key: string,
  value: string,
) {
  return invoke<LearningReviewSnapshot>("runtime_learning_propose", {
    repo,
    scope,
    key,
    value,
  });
}

export function evaluateLearning(
  repo: string,
  id: string,
  validationPassed: boolean,
  regressionPassed: boolean,
  effectivenessPassed: boolean,
) {
  return invoke<LearningReviewSnapshot>("runtime_learning_evaluate", {
    repo,
    id,
    validationPassed,
    regressionPassed,
    effectivenessPassed,
  });
}

export function saveLearningPrivacy(
  repo: string,
  privacy: LearningPrivacy,
  expectedRevision: number,
) {
  return invoke<LearningReviewSnapshot>("runtime_learning_privacy", {
    repo,
    privacy,
    expectedRevision,
  });
}

export function previewLearningExport(repo: string) {
  return invoke<RedactionPreview>("runtime_learning_redaction_preview", { repo });
}

export function exportLearningAudit(repo: string) {
  return invoke<unknown>("runtime_learning_export", { repo });
}
