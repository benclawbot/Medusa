import { describe, expect, it } from "vitest";

import {
  createDesktopCodingWorkflow,
  deserializeWorkflow,
  serializeWorkflow,
  transitionWorkflow,
  type DesktopCodingWorkflow,
} from "./workflow";

function advanceToApproval(): DesktopCodingWorkflow {
  let workflow = createDesktopCodingWorkflow("/workspace/medusa");
  workflow = transitionWorkflow(workflow, { type: "sessionReady", sessionId: "session-1" });
  workflow = transitionWorkflow(workflow, {
    type: "planPrepared",
    affectedFiles: ["src/b.ts", "src/a.ts", "src/a.ts"],
  });
  workflow = transitionWorkflow(workflow, { type: "approvalRequested" });
  return workflow;
}

describe("desktop coding workflow", () => {
  it("enforces the complete repository-to-pull-request journey", () => {
    let workflow = advanceToApproval();
    workflow = transitionWorkflow(workflow, {
      type: "planApproved",
      revision: workflow.planRevision,
    });
    workflow = transitionWorkflow(workflow, { type: "editingStarted" });
    workflow = transitionWorkflow(workflow, {
      type: "verificationStarted",
      checks: [{ name: "desktop tests", status: "running" }],
    });
    workflow = transitionWorkflow(workflow, {
      type: "verificationUpdated",
      checks: [{ name: "desktop tests", status: "passed" }],
    });
    workflow = transitionWorkflow(workflow, {
      type: "checkpointCreated",
      checkpointId: "checkpoint-1",
    });
    workflow = transitionWorkflow(workflow, {
      type: "readyToCommit",
      branch: "feature/workflow",
    });
    workflow = transitionWorkflow(workflow, { type: "committed", commitSha: "abc123" });
    workflow = transitionWorkflow(workflow, { type: "pushed" });
    workflow = transitionWorkflow(workflow, {
      type: "pullRequestOpened",
      url: "https://github.com/acme/medusa/pull/1",
    });
    workflow = transitionWorkflow(workflow, { type: "completed" });

    expect(workflow.phase).toBe("completed");
    expect(workflow.affectedFiles).toEqual(["src/a.ts", "src/b.ts"]);
    expect(workflow.checkpointId).toBe("checkpoint-1");
    expect(workflow.commitSha).toBe("abc123");
  });

  it("invalidates approval whenever the plan changes", () => {
    let workflow = advanceToApproval();
    workflow = transitionWorkflow(workflow, {
      type: "planApproved",
      revision: workflow.planRevision,
    });
    workflow = transitionWorkflow(workflow, {
      type: "planModified",
      affectedFiles: ["src/revised.ts"],
    });
    workflow = transitionWorkflow(workflow, { type: "approvalRequested" });

    expect(workflow.approvedPlanRevision).toBeUndefined();
    expect(() => transitionWorkflow(workflow, { type: "editingStarted" })).toThrow(
      "active plan revision has not been approved",
    );
  });

  it("rejects stale approval revisions", () => {
    const workflow = advanceToApproval();

    expect(() =>
      transitionWorkflow(workflow, {
        type: "planApproved",
        revision: workflow.planRevision - 1,
      }),
    ).toThrow("Approval does not match the active plan revision");
  });

  it("requires successful verification before checkpoint and commit", () => {
    let workflow = advanceToApproval();
    workflow = transitionWorkflow(workflow, {
      type: "planApproved",
      revision: workflow.planRevision,
    });
    workflow = transitionWorkflow(workflow, { type: "editingStarted" });
    workflow = transitionWorkflow(workflow, {
      type: "verificationStarted",
      checks: [{ name: "cargo test", status: "failed", details: "one failure" }],
    });

    expect(() =>
      transitionWorkflow(workflow, {
        type: "checkpointCreated",
        checkpointId: "checkpoint-1",
      }),
    ).toThrow("All verification checks must pass");
  });

  it("round-trips durable state for desktop resume", () => {
    let workflow = advanceToApproval();
    workflow = transitionWorkflow(workflow, {
      type: "planApproved",
      revision: workflow.planRevision,
    });

    expect(deserializeWorkflow(serializeWorkflow(workflow))).toEqual(workflow);
  });

  it("rejects malformed persisted workflow data", () => {
    expect(() => deserializeWorkflow('{"version":2}')).toThrow(
      "Unsupported or invalid desktop coding workflow state",
    );
  });
});
