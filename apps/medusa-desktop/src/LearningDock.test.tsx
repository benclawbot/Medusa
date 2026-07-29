import { describe, expect, it } from "vitest";
import type { LearningReviewItem } from "./learningApi";

function item(state: LearningReviewItem["state"]): LearningReviewItem {
  return {
    id: "lesson-1",
    revision: 1,
    state,
    kind: "skill",
    title: "Completeness gate",
    sourceSignalIds: ["signal-1"],
    evidenceDigests: ["a".repeat(64)],
    rootCause: "coverage was incomplete",
    generalizedRule: "inventory authoritative sources",
    scope: "repository",
    confidenceMilli: 900,
    proposedSolution: "workflow gate",
    nonApplicableContexts: [],
    conflictsWith: [],
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  };
}

describe("learning lifecycle contracts", () => {
  it("uses the backend serialized state names", () => {
    expect(item("rolled_back").state).toBe("rolled_back");
    expect(item("conflict").state).toBe("conflict");
  });

  it("keeps evidence references as digests rather than raw content", () => {
    const value = item("proposed");
    expect(value.evidenceDigests[0]).toHaveLength(64);
    expect(JSON.stringify(value)).not.toContain("data:image/");
    expect(JSON.stringify(value)).not.toContain("microphone transcript");
  });
});
