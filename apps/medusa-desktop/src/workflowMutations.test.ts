import { describe, expect, it } from "vitest";

import {
  createDesktopCodingWorkflow,
  transitionWorkflow,
  type DesktopCodingWorkflow,
} from "./workflow";
import {
  applyBranchResult,
  applyCheckpointResult,
  applyCommitResult,
  applyPushResult,
} from "./workflowMutations";

function verifiedWorkflow(): DesktopCodingWorkflow {
  let workflow = createDesktopCodingWorkflow("/repo");
  workflow = transitionWorkflow(workflow, { type: "sessionReady", sessionId: "session-1" });
  workflow = transitionWorkflow(workflow, {
    type: "planPrepared",
    affectedFiles: ["src/main.ts"],
  });
  workflow = transitionWorkflow(workflow, { type: "approvalRequested" });
  workflow = transitionWorkflow(workflow, {
    type: "planApproved",
    revision: workflow.planRevision,
  });
  workflow = transitionWorkflow(workflow, { type: "editingStarted" });
  workflow = transitionWorkflow(workflow, {
    type: "verificationStarted",
    checks: [{ name: "desktop tests", status: "passed" }],
  });
  return workflow;
}

describe("workflow mutation binding", () => {
  it("advances only from matching successful mutation results", () => {
    let workflow = verifiedWorkflow();
    workflow = applyCheckpointResult(workflow, {
      branch: "main",
      commitSha: "base123",
      checkpointRef: "refs/medusa/checkpoints/session-1",
    });
    workflow = applyBranchResult(workflow, {
      branch: "feature/safe",
      commitSha: "base123",
    });
    workflow = applyCommitResult(workflow, {
      branch: "feature/safe",
      commitSha: "commit456",
    });
    workflow = applyPushResult(workflow, {
      branch: "feature/safe",
      commitSha: "commit456",
    });

    expect(workflow.phase).toBe("pushed");
    expect(workflow.checkpointId).toBe("refs/medusa/checkpoints/session-1");
    expect(workflow.branch).toBe("feature/safe");
    expect(workflow.commitSha).toBe("commit456");
  });

  it("rejects a commit result from another branch", () => {
    let workflow = applyCheckpointResult(verifiedWorkflow(), {
      branch: "main",
      commitSha: "base123",
      checkpointRef: "refs/medusa/checkpoints/session-1",
    });
    workflow = applyBranchResult(workflow, {
      branch: "feature/safe",
      commitSha: "base123",
    });

    expect(() =>
      applyCommitResult(workflow, {
        branch: "feature/other",
        commitSha: "commit456",
      }),
    ).toThrow("Commit result branch does not match");
  });

  it("rejects a push result for a different commit", () => {
    let workflow = applyCheckpointResult(verifiedWorkflow(), {
      branch: "main",
      commitSha: "base123",
      checkpointRef: "refs/medusa/checkpoints/session-1",
    });
    workflow = applyBranchResult(workflow, {
      branch: "feature/safe",
      commitSha: "base123",
    });
    workflow = applyCommitResult(workflow, {
      branch: "feature/safe",
      commitSha: "commit456",
    });

    expect(() =>
      applyPushResult(workflow, {
        branch: "feature/safe",
        commitSha: "different",
      }),
    ).toThrow("Push result commit does not match");
  });
});
