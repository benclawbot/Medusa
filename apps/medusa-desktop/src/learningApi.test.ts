import { describe, expect, it } from "vitest";
import type { LearningPrivacy } from "./learningApi";

describe("learning privacy contract", () => {
  it("represents private-by-default settings without cross-repository reuse or telemetry", () => {
    const privacy: LearningPrivacy = {
      captureEnabled: true,
      userPersistenceEnabled: false,
      crossRepositoryReuseEnabled: false,
      telemetryEnabled: false,
      automaticProposalsEnabled: true,
    };
    expect(privacy.captureEnabled).toBe(true);
    expect(privacy.userPersistenceEnabled).toBe(false);
    expect(privacy.crossRepositoryReuseEnabled).toBe(false);
    expect(privacy.telemetryEnabled).toBe(false);
  });
});
