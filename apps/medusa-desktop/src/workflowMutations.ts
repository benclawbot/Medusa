import type { GitMutationResult } from "./gitMutations";
import {
  transitionWorkflow,
  type DesktopCodingWorkflow,
} from "./workflow";

export function applyBranchResult(
  workflow: DesktopCodingWorkflow,
  result: GitMutationResult,
): DesktopCodingWorkflow {
  if (workflow.phase !== "checkpointed") {
    throw new Error("Branch creation can only advance a checkpointed workflow.");
  }
  if (!result.branch.trim()) {
    throw new Error("Branch creation did not return an active branch.");
  }
  return transitionWorkflow(workflow, {
    type: "readyToCommit",
    branch: result.branch,
  });
}

export function applyCheckpointResult(
  workflow: DesktopCodingWorkflow,
  result: GitMutationResult,
): DesktopCodingWorkflow {
  if (workflow.phase !== "verifying") {
    throw new Error("Checkpoint creation can only advance a verified workflow.");
  }
  if (!result.checkpointRef?.trim()) {
    throw new Error("Checkpoint creation did not return a checkpoint reference.");
  }
  return transitionWorkflow(workflow, {
    type: "checkpointCreated",
    checkpointId: result.checkpointRef,
  });
}

export function applyCommitResult(
  workflow: DesktopCodingWorkflow,
  result: GitMutationResult,
): DesktopCodingWorkflow {
  if (workflow.phase !== "readyToCommit") {
    throw new Error("Commit can only advance a workflow that is ready to commit.");
  }
  if (workflow.branch !== result.branch) {
    throw new Error("Commit result branch does not match the workflow branch.");
  }
  if (!result.commitSha.trim()) {
    throw new Error("Commit did not return a commit SHA.");
  }
  return transitionWorkflow(workflow, {
    type: "committed",
    commitSha: result.commitSha,
  });
}

export function applyPushResult(
  workflow: DesktopCodingWorkflow,
  result: GitMutationResult,
): DesktopCodingWorkflow {
  if (workflow.phase !== "committed") {
    throw new Error("Push can only advance a committed workflow.");
  }
  if (workflow.branch !== result.branch) {
    throw new Error("Push result branch does not match the workflow branch.");
  }
  if (workflow.commitSha !== result.commitSha) {
    throw new Error("Push result commit does not match the committed workflow state.");
  }
  return transitionWorkflow(workflow, { type: "pushed" });
}
