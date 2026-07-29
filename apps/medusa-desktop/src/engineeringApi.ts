import { invoke } from "@tauri-apps/api/core";

export interface EngineeringPoint {
  date: string;
  total: number;
  successful: number;
  successRate: number;
}

export interface FrictionItem {
  category: string;
  count: number;
  sessions: string[];
}

export interface ImprovementRecord {
  id: string;
  createdAt: string;
  updatedAt: string;
  title: string;
  problem: string;
  proposedChange: string;
  evidence: string[];
  sourceSessions: string[];
  risk: string;
  status: string;
  benchmarkBefore?: number;
  benchmarkAfter?: number;
  rollbackNote: string;
  revision: number;
  approval?: { reviewer: string; approvedAt: string; proposalRevision: number };
  activeVersion?: string;
  previousVersion?: string;
  conflictsWith: string[];
  observations: Array<{ observedAt: string; triggerCount: number; correctionCount: number; regressionCount: number; latencyMs?: number }>;
  suspensionReason?: string;
}

export interface LearningPrivacySettings {
  captureEnabled: boolean;
  repositoryPersistence: boolean;
  crossRepositoryReuse: boolean;
  telemetryEnabled: boolean;
  automaticProposals: boolean;
}

export interface EngineeringDashboardData {
  totalTasks: number;
  successfulTasks: number;
  successRate: number;
  verificationPassRate: number;
  averageRetries: number;
  humanInterventionRate: number;
  rollbackRate: number;
  averageDurationMinutes: number;
  trend: EngineeringPoint[];
  friction: FrictionItem[];
  improvements: ImprovementRecord[];
  generatedAt: string;
}

export function loadEngineeringDashboard(repo: string, days = 90) {
  return invoke<EngineeringDashboardData>("runtime_engineering_dashboard", { repo, days });
}

export function generateImprovement(repo: string) {
  return invoke<ImprovementRecord>("runtime_generate_improvement", { repo });
}

export function updateImprovement(
  repo: string,
  id: string,
  action: "approve" | "reject" | "adopt" | "rollback" | "benchmark" | "suspend" | "supersede",
) {
  return invoke<ImprovementRecord>("runtime_update_improvement", { repo, id, action });
}

export function loadLearningPrivacy(repo: string) {
  return invoke<LearningPrivacySettings>("runtime_learning_privacy", { repo });
}

export function updateLearningPrivacy(repo: string, settings: LearningPrivacySettings) {
  return invoke<LearningPrivacySettings>("runtime_update_learning_privacy", { repo, settings });
}

export function redactImprovement(repo: string, id: string) {
  return invoke<void>("runtime_redact_improvement", { repo, id });
}

export function deleteImprovement(repo: string, id: string) {
  return invoke<void>("runtime_delete_improvement", { repo, id });
}

export function exportLearningAudit(repo: string) {
  return invoke<string>("runtime_export_learning_audit", { repo });
}
