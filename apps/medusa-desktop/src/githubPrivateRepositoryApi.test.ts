import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  cloneGitHubRepository,
  fetchGitHubRepository,
} from "./githubPrivateRepositoryApi";

describe("GitHub private repository transfer", () => {
  beforeEach(() => invoke.mockReset());

  it("normalizes clone invocation without embedding credentials", async () => {
    invoke.mockResolvedValue({
      state: "ready",
      repository: "octo/private",
      path: "/tmp/private",
      operation: "clone",
      message: "GitHub repository cloned",
    });

    await cloneGitHubRepository(
      " octo/private ",
      " /tmp/private ",
      " github.com ",
    );

    expect(invoke).toHaveBeenCalledWith("runtime_clone_github_repository", {
      repository: "octo/private",
      destination: "/tmp/private",
      hostname: "github.com",
    });
  });

  it("normalizes fetch invocation", async () => {
    invoke.mockResolvedValue({
      state: "ready",
      repository: "octo/private",
      path: "/tmp/private",
      operation: "fetch",
      message: "GitHub repository fetched",
    });

    await fetchGitHubRepository("octo/private", "/tmp/private");

    expect(invoke).toHaveBeenCalledWith("runtime_fetch_github_repository", {
      repository: "octo/private",
      localPath: "/tmp/private",
      hostname: null,
    });
  });

  it("rejects blank targets before invoking Tauri", async () => {
    await expect(cloneGitHubRepository("", "/tmp/private")).rejects.toThrow(
      "repository is required",
    );
    await expect(fetchGitHubRepository("octo/private", " ")).rejects.toThrow(
      "path is required",
    );
    expect(invoke).not.toHaveBeenCalled();
  });
});
