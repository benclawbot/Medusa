import { invoke } from "@tauri-apps/api/core";

export interface GitHubAuditReceipt {
  operation: string;
  repository: string;
  resource: string;
  previewFingerprint: string;
  confirmedAt: string;
  outcome: string;
}

export async function persistGitHubAudit(receipt: GitHubAuditReceipt): Promise<void> {
  await invoke("runtime_persist_github_audit", { receipt });
}
