import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { retryGitHubActionsJob } from "./githubActionsApi";

const preview = {
  kind: "actionsRetry" as const,
  repository: "octo/repo",
  branch: "abcdef1",
  title: "Retry failed GitHub Actions job",
  body: "Retry job 99 from run 42",
  recipients: [],
  affectedResources: ["actionsRun:42", "actionsJob:99"],
  destructive: false,
};

const confirmation = {
  previewFingerprint: "fingerprint",
  confirmedAt: "2026-07-22T00:00:00.000Z",
};

describe("retryGitHubActionsJob", () => {
  beforeEach(() => invoke.mockReset());

  it("invokes the confirmed retry command with normalized target fields", async () => {
    invoke.mockResolvedValue({
      repository: "octo/repo",
      runId: 42,
      jobId: 99,
      commitSha: "abcdef1",
      audit: {
        operation: "actionsRetry",
        repository: "octo/repo",
        runId: 42,
        jobId: 99,
        commitSha: "abcdef1",
        previewFingerprint: "fingerprint",
        confirmedAt: "2026-07-22T00:00:00.000Z",
        outcome: "requested",
      },
    });

    await retryGitHubActionsJob(
      " octo/repo ",
      42,
      99,
      " abcdef1 ",
      preview,
      confirmation,
      " github.com ",
    );

    expect(invoke).toHaveBeenCalledWith("runtime_retry_github_actions_job", {
      repository: "octo/repo",
      hostname: "github.com",
      runId: 42,
      jobId: 99,
      commitSha: "abcdef1",
      preview,
      confirmation,
    });
  });

  it("rejects the wrong mutation kind before invoking Tauri", async () => {
    await expect(
      retryGitHubActionsJob(
        "octo/repo",
        42,
        99,
        "abcdef1",
        { ...preview, kind: "push" },
        confirmation,
      ),
    ).rejects.toThrow("requires an actionsRetry preview");
    expect(invoke).not.toHaveBeenCalled();
  });
});
