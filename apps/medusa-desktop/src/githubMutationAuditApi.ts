import { invoke } from "@tauri-apps/api/core";

export interface GitHubMutationAuditReceipt {
  operation: string;
  repository: string;
  resource: string;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: string;
}

export interface GitHubMutationAuditPersistence {
  persisted: boolean;
  receiptPath: string;
}

export async function persistGitHubMutationAudit(
  receipt: GitHubMutationAuditReceipt,
): Promise<GitHubMutationAuditPersistence> {
  const normalized = {
    ...receipt,
    operation: receipt.operation.trim(),
    repository: receipt.repository.trim(),
    resource: receipt.resource.trim(),
    previewFingerprint: receipt.previewFingerprint.trim(),
    confirmedAt: receipt.confirmedAt.trim(),
    outcome: receipt.outcome.trim(),
  };
  if (Object.values(normalized).some((value) => !value)) {
    throw new Error("GitHub mutation audit receipt fields are required");
  }
  return invoke<GitHubMutationAuditPersistence>("runtime_persist_github_mutation_audit", {
    receipt: normalized,
  });
}
