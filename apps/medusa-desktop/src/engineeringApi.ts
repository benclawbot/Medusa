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
