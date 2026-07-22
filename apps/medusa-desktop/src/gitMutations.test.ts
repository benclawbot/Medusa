import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import {
  commitChanges,
  createBranch,
  createCheckpoint,
  pushBranch,
} from "./gitMutations";
import { confirmMutation, type GitHubMutationPreview } from "./githubMutation";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);

function preview(kind: GitHubMutationPreview["kind"]): GitHubMutationPreview {
  return {
    kind,
    repository: "/repo",
    branch: "feature/safe",
    title: `${kind} verified changes`,
    recipients: [],
    affectedResources: ["file:src/main.rs"],
    destructive: false,
  };
}

describe("confirmed git mutation adapter", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("passes the exact branch preview and confirmation to Tauri", async () => {
    const value = preview("branch");
    const confirmation = confirmMutation(value, "2026-07-22T12:00:00.000Z");
    mockedInvoke.mockResolvedValueOnce({ branch: "feature/safe", commitSha: "abc" });

    await createBranch("/repo", "feature/safe", value, confirmation);

    expect(mockedInvoke).toHaveBeenCalledWith("runtime_create_branch", {
      repo: "/repo",
      branch: "feature/safe",
      preview: value,
      confirmation,
    });
  });

  it("uses explicit paths for commits", async () => {
    const value = preview("commit");
    const confirmation = confirmMutation(value);
    mockedInvoke.mockResolvedValueOnce({ branch: "feature/safe", commitSha: "def" });

    await commitChanges("/repo", "feat: safe change", ["src/main.rs"], value, confirmation);

    expect(mockedInvoke).toHaveBeenCalledWith("runtime_commit_changes", {
      repo: "/repo",
      message: "feat: safe change",
      paths: ["src/main.rs"],
      preview: value,
      confirmation,
    });
  });

  it("exposes checkpoint and push operations without implicit defaults in the preview", async () => {
    const checkpoint = preview("checkpoint");
    const checkpointConfirmation = confirmMutation(checkpoint);
    mockedInvoke.mockResolvedValueOnce({
      branch: "feature/safe",
      commitSha: "abc",
      checkpointRef: "refs/medusa/checkpoints/before-edit",
    });
    await createCheckpoint("/repo", "before-edit", checkpoint, checkpointConfirmation);

    const push = preview("push");
    const pushConfirmation = confirmMutation(push);
    mockedInvoke.mockResolvedValueOnce({ branch: "feature/safe", commitSha: "abc" });
    await pushBranch("/repo", push, pushConfirmation, "origin");

    expect(mockedInvoke).toHaveBeenNthCalledWith(1, "runtime_create_checkpoint", {
      repo: "/repo",
      checkpointRef: "before-edit",
      preview: checkpoint,
      confirmation: checkpointConfirmation,
    });
    expect(mockedInvoke).toHaveBeenNthCalledWith(2, "runtime_push_branch", {
      repo: "/repo",
      remote: "origin",
      preview: push,
      confirmation: pushConfirmation,
    });
  });
});
