import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { readGithubAuthStatus } from "./githubAuthApi";

describe("readGithubAuthStatus", () => {
  beforeEach(() => invoke.mockReset());

  it("invokes the typed GitHub auth status command", async () => {
    invoke.mockResolvedValue({
      state: "ready",
      hostname: "github.com",
      account: "octocat",
      scopes: ["read:org", "repo", "workflow"],
      missingScopes: [],
      message: "GitHub authentication is ready",
    });

    await expect(readGithubAuthStatus()).resolves.toMatchObject({
      state: "ready",
      account: "octocat",
    });

    expect(invoke).toHaveBeenCalledWith("runtime_github_auth_status", {
      hostname: "github.com",
    });
  });

  it("normalizes custom hostnames", async () => {
    invoke.mockResolvedValue({ state: "unauthenticated" });

    await readGithubAuthStatus("  github.example.com  ");

    expect(invoke).toHaveBeenCalledWith("runtime_github_auth_status", {
      hostname: "github.example.com",
    });
  });

  it("rejects an empty hostname before invoking Tauri", async () => {
    await expect(readGithubAuthStatus("   ")).rejects.toThrow(
      "GitHub hostname is required",
    );
    expect(invoke).not.toHaveBeenCalled();
  });
});
