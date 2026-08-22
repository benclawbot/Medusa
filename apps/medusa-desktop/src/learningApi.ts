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

type WireRecord = Record<string, unknown>;

function asRecord(value: unknown): WireRecord {
  return value && typeof value === "object" ? value as WireRecord : {};
}

function pick(record: WireRecord, camel: string, snake = camel): unknown {
  return record[camel] ?? record[snake];
}

function stringValue(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function numberValue(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function booleanValue(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function normalizeReplay(value: unknown): LearningReviewItem["replay"] {
  if (!value || typeof value !== "object") return undefined;
  const replay = asRecord(value);
  return {
    reproduced: booleanValue(pick(replay, "reproduced")),
    resolved: booleanValue(pick(replay, "resolved")),
    regressionCount: numberValue(pick(replay, "regressionCount", "regression_count")),
    evidenceDigests: stringArray(pick(replay, "evidenceDigests", "evidence_digests")),
  };
}

const learningStates = new Set<LearningReviewState>([
  "proposed", "deferred", "approved", "rejected", "validated", "active",
  "suspended", "rolled_back", "deleted", "conflict",
]);

function normalizeState(value: unknown): LearningReviewState {
  const state = stringValue(value);
  return learningStates.has(state as LearningReviewState) ? state as LearningReviewState : "proposed";
}

function normalizeLearningItem(value: unknown): LearningReviewItem {
  const item = asRecord(value);
  return {
    id: stringValue(pick(item, "id")),
    revision: numberValue(pick(item, "revision")),
    state: normalizeState(pick(item, "state")),
    kind: stringValue(pick(item, "kind"), "unknown"),
    title: stringValue(pick(item, "title")),
    sourceSignalIds: stringArray(pick(item, "sourceSignalIds", "source_signal_ids")),
    evidenceDigests: stringArray(pick(item, "evidenceDigests", "evidence_digests")),
    rootCause: stringValue(pick(item, "rootCause", "root_cause")),
    generalizedRule: stringValue(pick(item, "generalizedRule", "generalized_rule")),
    scope: stringValue(pick(item, "scope")),
    confidenceMilli: numberValue(pick(item, "confidenceMilli", "confidence_milli")),
    proposedSolution: stringValue(pick(item, "proposedSolution", "proposed_solution")),
    nonApplicableContexts: stringArray(pick(item, "nonApplicableContexts", "non_applicable_contexts")),
    replay: normalizeReplay(pick(item, "replay")),
    conflictsWith: stringArray(pick(item, "conflictsWith", "conflicts_with")),
    activeVersion: typeof pick(item, "activeVersion", "active_version") === "string" ? pick(item, "activeVersion", "active_version") as string : undefined,
    previousVersion: typeof pick(item, "previousVersion", "previous_version") === "string" ? pick(item, "previousVersion", "previous_version") as string : undefined,
    createdAtUnixMs: numberValue(pick(item, "createdAtUnixMs", "created_at_unix_ms")),
    updatedAtUnixMs: numberValue(pick(item, "updatedAtUnixMs", "updated_at_unix_ms")),
  };
}

function normalizePrivacy(value: unknown): LearningPrivacy {
  const privacy = asRecord(value);
  return {
    captureEnabled: booleanValue(pick(privacy, "captureEnabled", "capture_enabled")),
    userPersistenceEnabled: booleanValue(pick(privacy, "userPersistenceEnabled", "user_persistence_enabled")),
    crossRepositoryReuseEnabled: booleanValue(pick(privacy, "crossRepositoryReuseEnabled", "cross_repository_reuse_enabled")),
    telemetryEnabled: booleanValue(pick(privacy, "telemetryEnabled", "telemetry_enabled")),
    automaticProposalsEnabled: booleanValue(pick(privacy, "automaticProposalsEnabled", "automatic_proposals_enabled")),
  };
}

export function normalizeLearningReviewSnapshot(value: unknown): LearningReviewSnapshot {
  const snapshot = asRecord(value);
  return {
    schemaVersion: numberValue(pick(snapshot, "schemaVersion", "schema_version")),
    revision: numberValue(pick(snapshot, "revision")),
    privacy: normalizePrivacy(pick(snapshot, "privacy")),
    items: Array.isArray(pick(snapshot, "items")) ? (pick(snapshot, "items") as unknown[]).map(normalizeLearningItem) : [],
    auditHead: stringValue(pick(snapshot, "auditHead", "audit_head")),
  };
}

function normalizeRedactionPreview(value: unknown): RedactionPreview {
  const preview = asRecord(value);
  return {
    safe: booleanValue(pick(preview, "safe")),
    blockedFields: stringArray(pick(preview, "blockedFields", "blocked_fields")),
    warnings: stringArray(pick(preview, "warnings")),
    itemCount: numberValue(pick(preview, "itemCount", "item_count")),
  };
}

export async function loadLearningReview(repo: string): Promise<LearningReviewSnapshot> {
  return normalizeLearningReviewSnapshot(await invoke<unknown>("runtime_learning_review", { repo }));
}

export function transitionLearning(
  repo: string,
  id: string,
  action: "approve" | "reject" | "defer" | "validate" | "activate" | "suspend" | "rollback" | "delete",
  expectedRevision: number,
) {
  return invoke<unknown>("runtime_learning_transition", {
    repo,
    id,
    action,
    expectedRevision,
  }).then(normalizeLearningReviewSnapshot);
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
  return invoke<unknown>("runtime_learning_propose", {
    repo,
    scope,
    key,
    value,
  }).then(normalizeLearningReviewSnapshot);
}

export function evaluateLearning(
  repo: string,
  id: string,
  validationPassed: boolean,
  regressionPassed: boolean,
  effectivenessPassed: boolean,
) {
  return invoke<unknown>("runtime_learning_evaluate", {
    repo,
    id,
    validationPassed,
    regressionPassed,
    effectivenessPassed,
  }).then(normalizeLearningReviewSnapshot);
}

export function saveLearningPrivacy(
  repo: string,
  privacy: LearningPrivacy,
  expectedRevision: number,
) {
  return invoke<unknown>("runtime_learning_privacy", {
    repo,
    privacy,
    expectedRevision,
  }).then(normalizeLearningReviewSnapshot);
}

export function previewLearningExport(repo: string) {
  return invoke<unknown>("runtime_learning_redaction_preview", { repo }).then(normalizeRedactionPreview);
}

export function exportLearningAudit(repo: string) {
  return invoke<unknown>("runtime_learning_export", { repo });
}
