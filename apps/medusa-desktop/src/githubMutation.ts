export type GitHubMutationKind =
  | "branch"
  | "checkpoint"
  | "commit"
  | "push"
  | "pullRequest";

export interface GitHubMutationPreview {
  kind: GitHubMutationKind;
  repository: string;
  branch: string;
  title: string;
  body?: string;
  recipients: string[];
  affectedResources: string[];
  destructive: boolean;
}

export interface GitHubMutationConfirmation {
  previewFingerprint: string;
  confirmedAt: string;
}

export function createMutationFingerprint(preview: GitHubMutationPreview): string {
  return JSON.stringify({
    kind: preview.kind,
    repository: preview.repository.trim(),
    branch: preview.branch.trim(),
    title: preview.title.trim(),
    body: preview.body?.trim() ?? "",
    recipients: [...preview.recipients].map((value) => value.trim()).sort(),
    affectedResources: [...preview.affectedResources].map((value) => value.trim()).sort(),
    destructive: preview.destructive,
  });
}

export function validateMutationPreview(preview: GitHubMutationPreview): string[] {
  const errors: string[] = [];
  if (!preview.repository.trim()) errors.push("repository is required");
  if (!preview.branch.trim()) errors.push("branch is required");
  if (!preview.title.trim()) errors.push("title is required");
  if (preview.kind === "pullRequest" && !preview.body?.trim()) {
    errors.push("pull request body is required");
  }
  if (preview.affectedResources.length === 0) {
    errors.push("at least one affected resource is required");
  }
  if (preview.recipients.some((value) => !value.trim())) {
    errors.push("recipients cannot contain blank values");
  }
  if (preview.affectedResources.some((value) => !value.trim())) {
    errors.push("affected resources cannot contain blank values");
  }
  return errors;
}

export function confirmMutation(
  preview: GitHubMutationPreview,
  confirmedAt = new Date().toISOString(),
): GitHubMutationConfirmation {
  const errors = validateMutationPreview(preview);
  if (errors.length > 0) {
    throw new Error(`Cannot confirm GitHub mutation: ${errors.join(", ")}`);
  }
  return {
    previewFingerprint: createMutationFingerprint(preview),
    confirmedAt,
  };
}

export function confirmationMatchesPreview(
  preview: GitHubMutationPreview,
  confirmation: GitHubMutationConfirmation | undefined,
): boolean {
  return confirmation?.previewFingerprint === createMutationFingerprint(preview);
}
