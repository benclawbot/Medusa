import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { createDraftPullRequest } from "./pullRequestApi";

const preview = {
  kind: "pullRequest" as const,
  repository: "/repo",
  branch: "feature/safe",
  title: "Open draft pull request",
  body: "Summary and validation",
  recipients: ["reviewer"],
  affectedResources: ["branch:feature/safe"],
  destructive: false,
};

const confirmation = {
  previewFingerprint: "fingerprint",
  confirmedAt: "2026-07-22T00:00:00.000Z",
};

describe("createDraftPullRequest", () => {
  beforeEach(() => invoke.mockReset());

  it("invokes the confirmed draft pull request command", async () => {
    invoke.mockResolvedValue({
      branch: "feature/safe",
      commitSha: "abc123",
      pullRequestUrl: "https://github.com/example/repo/pull/1",
    });

    await expect(
      createDraftPullRequest("/repo", "main", preview, confirmation),
    ).resolves.toMatchObject({ commitSha: "abc123" });

    expect(invoke).toHaveBeenCalledWith("runtime_create_draft_pull_request", {
      repo: "/repo",
      base: "main",
      preview,
      confirmation,
    });
  });

  it("rejects a non-pull-request preview before invoking Tauri", async () => {
    await expect(
      createDraftPullRequest(
        "/repo",
        "main",
        { ...preview, kind: "commit" },
        confirmation,
      ),
    ).rejects.toThrow("requires a pullRequest preview");
    expect(invoke).not.toHaveBeenCalled();
  });
});
