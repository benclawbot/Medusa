export type WorkflowPhase =
  | "repositorySelected"
  | "sessionReady"
  | "planned"
  | "awaitingApproval"
  | "editing"
  | "verifying"
  | "checkpointed"
  | "readyToCommit"
  | "committed"
  | "pushed"
  | "pullRequestOpened"
  | "completed"
  | "failed";

export interface WorkflowCheck {
  name: string;
  status: "pending" | "running" | "passed" | "failed";
  details?: string;
}

export interface DesktopCodingWorkflow {
  version: 1;
  repository: string;
  phase: WorkflowPhase;
  sessionId?: string;
  planRevision: number;
  approvedPlanRevision?: number;
  affectedFiles: string[];
  checkpointId?: string;
  branch?: string;
  commitSha?: string;
  pullRequestUrl?: string;
  checks: WorkflowCheck[];
  failure?: string;
}

export type WorkflowEvent =
  | { type: "sessionReady"; sessionId: string }
  | { type: "planPrepared"; affectedFiles: string[] }
  | { type: "approvalRequested" }
  | { type: "planApproved"; revision: number }
  | { type: "planModified"; affectedFiles: string[] }
  | { type: "editingStarted" }
  | { type: "verificationStarted"; checks: WorkflowCheck[] }
  | { type: "verificationUpdated"; checks: WorkflowCheck[] }
  | { type: "checkpointCreated"; checkpointId: string }
  | { type: "readyToCommit"; branch: string }
  | { type: "committed"; commitSha: string }
  | { type: "pushed" }
  | { type: "pullRequestOpened"; url: string }
  | { type: "completed" }
  | { type: "failed"; message: string };

export function createDesktopCodingWorkflow(repository: string): DesktopCodingWorkflow {
  const normalizedRepository = repository.trim();
  if (!normalizedRepository) {
    throw new Error("A repository is required to start the desktop coding workflow.");
  }

  return {
    version: 1,
    repository: normalizedRepository,
    phase: "repositorySelected",
    planRevision: 0,
    affectedFiles: [],
    checks: [],
  };
}

export function transitionWorkflow(
  workflow: DesktopCodingWorkflow,
  event: WorkflowEvent,
): DesktopCodingWorkflow {
  if (event.type === "failed") {
    return { ...workflow, phase: "failed", failure: event.message };
  }

  switch (event.type) {
    case "sessionReady":
      requirePhase(workflow, ["repositorySelected"]);
      return { ...workflow, phase: "sessionReady", sessionId: event.sessionId };
    case "planPrepared":
      requirePhase(workflow, ["sessionReady", "planned", "awaitingApproval"]);
      return applyNewPlan(workflow, event.affectedFiles, "planned");
    case "approvalRequested":
      requirePhase(workflow, ["planned"]);
      return { ...workflow, phase: "awaitingApproval" };
    case "planApproved":
      requirePhase(workflow, ["awaitingApproval"]);
      if (event.revision !== workflow.planRevision) {
        throw new Error("Approval does not match the active plan revision.");
      }
      return { ...workflow, approvedPlanRevision: event.revision };
    case "planModified":
      requirePhase(workflow, ["planned", "awaitingApproval"]);
      return applyNewPlan(workflow, event.affectedFiles, "planned");
    case "editingStarted":
      requirePhase(workflow, ["awaitingApproval"]);
      requireCurrentApproval(workflow);
      return { ...workflow, phase: "editing" };
    case "verificationStarted":
      requirePhase(workflow, ["editing"]);
      return { ...workflow, phase: "verifying", checks: event.checks };
    case "verificationUpdated":
      requirePhase(workflow, ["verifying"]);
      return { ...workflow, checks: event.checks };
    case "checkpointCreated":
      requirePhase(workflow, ["verifying"]);
      requirePassingChecks(workflow.checks);
      return { ...workflow, phase: "checkpointed", checkpointId: event.checkpointId };
    case "readyToCommit":
      requirePhase(workflow, ["checkpointed"]);
      if (!workflow.checkpointId) {
        throw new Error("A rollback checkpoint is required before commit.");
      }
      return { ...workflow, phase: "readyToCommit", branch: event.branch };
    case "committed":
      requirePhase(workflow, ["readyToCommit"]);
      return { ...workflow, phase: "committed", commitSha: event.commitSha };
    case "pushed":
      requirePhase(workflow, ["committed"]);
      return { ...workflow, phase: "pushed" };
    case "pullRequestOpened":
      requirePhase(workflow, ["pushed"]);
      return { ...workflow, phase: "pullRequestOpened", pullRequestUrl: event.url };
    case "completed":
      requirePhase(workflow, ["pullRequestOpened"]);
      return { ...workflow, phase: "completed" };
  }
}

export function serializeWorkflow(workflow: DesktopCodingWorkflow): string {
  return JSON.stringify(workflow);
}

export function deserializeWorkflow(serialized: string): DesktopCodingWorkflow {
  const parsed = JSON.parse(serialized) as Partial<DesktopCodingWorkflow>;
  if (
    parsed.version !== 1 ||
    typeof parsed.repository !== "string" ||
    typeof parsed.phase !== "string" ||
    typeof parsed.planRevision !== "number" ||
    !Array.isArray(parsed.affectedFiles) ||
    !Array.isArray(parsed.checks)
  ) {
    throw new Error("Unsupported or invalid desktop coding workflow state.");
  }
  return parsed as DesktopCodingWorkflow;
}

function applyNewPlan(
  workflow: DesktopCodingWorkflow,
  affectedFiles: string[],
  phase: WorkflowPhase,
): DesktopCodingWorkflow {
  return {
    ...workflow,
    phase,
    planRevision: workflow.planRevision + 1,
    approvedPlanRevision: undefined,
    affectedFiles: [...new Set(affectedFiles)].sort(),
    checkpointId: undefined,
    branch: undefined,
    commitSha: undefined,
    pullRequestUrl: undefined,
    checks: [],
    failure: undefined,
  };
}

function requireCurrentApproval(workflow: DesktopCodingWorkflow): void {
  if (workflow.approvedPlanRevision !== workflow.planRevision) {
    throw new Error("The active plan revision has not been approved.");
  }
}

function requirePassingChecks(checks: WorkflowCheck[]): void {
  if (checks.length === 0 || checks.some((check) => check.status !== "passed")) {
    throw new Error("All verification checks must pass before creating a checkpoint.");
  }
}

function requirePhase(workflow: DesktopCodingWorkflow, allowed: WorkflowPhase[]): void {
  if (!allowed.includes(workflow.phase)) {
    throw new Error(`Cannot apply workflow transition from ${workflow.phase}.`);
  }
}
