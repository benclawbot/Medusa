import { describe, expect, it } from "vitest";

import {
  confirmationMatchesPreview,
  confirmMutation,
  createMutationFingerprint,
  type GitHubMutationPreview,
  validateMutationPreview,
} from "./githubMutation";

function preview(): GitHubMutationPreview {
  return {
    kind: "pullRequest",
    repository: "acme/medusa",
    branch: "feature/safe-workflow",
    title: "Complete desktop workflow",
    body: "Adds the repository-to-PR journey.",
    recipients: ["reviewers"],
    affectedResources: ["branch:feature/safe-workflow", "pull-request:draft"],
    destructive: false,
  };
}

describe("GitHub mutation previews", () => {
  it("requires all externally visible pull request details", () => {
    const invalid = {
      ...preview(),
      repository: " ",
      branch: "",
      title: "",
      body: "",
      affectedResources: [],
    };

    expect(validateMutationPreview(invalid)).toEqual([
      "repository is required",
      "branch is required",
      "title is required",
      "pull request body is required",
      "at least one affected resource is required",
    ]);
  });

  it("binds confirmation to the exact preview", () => {
    const original = preview();
    const confirmation = confirmMutation(original, "2026-07-22T12:00:00.000Z");

    expect(confirmationMatchesPreview(original, confirmation)).toBe(true);
    expect(
      confirmationMatchesPreview(
        { ...original, title: "Silently changed title" },
        confirmation,
      ),
    ).toBe(false);
  });

  it("normalizes ordering for stable durable fingerprints", () => {
    const original = preview();
    const reordered = {
      ...original,
      recipients: [...original.recipients].reverse(),
      affectedResources: [...original.affectedResources].reverse(),
    };

    expect(createMutationFingerprint(reordered)).toBe(createMutationFingerprint(original));
  });

  it("refuses confirmation for an incomplete preview", () => {
    expect(() => confirmMutation({ ...preview(), affectedResources: [] })).toThrow(
      "Cannot confirm GitHub mutation",
    );
  });
});
