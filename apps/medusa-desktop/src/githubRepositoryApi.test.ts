import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { readGithubRepositoryAccess } from "./githubRepositoryApi";

describe("readGithubRepositoryAccess", () => {
  beforeEach(() => invoke.mockReset());

  it("invokes the typed repository access command", async () => {
    invoke.mockResolvedValue({
      state: "ready",
      hostname: "github.com",
      repository: "octo/private",
      visibility: "private",
      defaultBranch: "main",
      permissions: ["push", "pull"],
      message: "GitHub repository access is ready",
    });

    await expect(
      readGithubRepositoryAccess(" octo/private ", " github.com "),
    ).resolves.toMatchObject({ state: "ready", visibility: "private" });

    expect(invoke).toHaveBeenCalledWith("runtime_github_repository_access", {
      repository: "octo/private",
      hostname: "github.com",
    });
  });

  it("passes a null hostname when the default should be used", async () => {
    invoke.mockResolvedValue({ state: "notFound" });

    await readGithubRepositoryAccess("octo/missing");

    expect(invoke).toHaveBeenCalledWith("runtime_github_repository_access", {
      repository: "octo/missing",
      hostname: null,
    });
  });
});
