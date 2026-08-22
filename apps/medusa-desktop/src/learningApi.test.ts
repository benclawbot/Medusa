import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { loadLearningReview, normalizeLearningReviewSnapshot } from "./learningApi";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mockedInvoke = vi.mocked(invoke);

describe("learning API wire normalization", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("normalizes the snake_case payload returned by the desktop command", async () => {
    mockedInvoke.mockResolvedValueOnce({
      schema_version: 1,
      revision: 7,
      privacy: {
        capture_enabled: true,
        user_persistence_enabled: false,
        cross_repository_reuse_enabled: false,
        telemetry_enabled: false,
        automatic_proposals_enabled: true,
      },
      items: [{
        id: "lesson-1",
        revision: 3,
        state: "proposed",
        kind: "repository_learning",
        title: "Keep the runtime evidence bounded",
        source_signal_ids: ["signal-1"],
        evidence_digests: ["a".repeat(64)],
        root_cause: "an unbounded feed",
        generalized_rule: "bound retained evidence",
        scope: "repository",
        confidence_milli: 900,
        proposed_solution: "apply a bounded projection",
        non_applicable_contexts: [],
        replay: {
          reproduced: true,
          resolved: true,
          regression_count: 2,
          evidence_digests: [],
        },
        conflicts_with: [],
        active_version: null,
        previous_version: null,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 2,
      }],
      audit_head: "hash",
    });

    const snapshot = await loadLearningReview("C:/repo");

    expect(snapshot.items[0]).toMatchObject({
      id: "lesson-1",
      generalizedRule: "bound retained evidence",
      conflictsWith: [],
      replay: { regressionCount: 2 },
    });
    expect(snapshot.privacy.captureEnabled).toBe(true);
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_learning_review", { repo: "C:/repo" });
  });

  it("defaults missing collection fields so malformed data cannot crash the panel", () => {
    const snapshot = normalizeLearningReviewSnapshot({
      items: [{ id: "lesson-2", title: "Incomplete item" }],
    });

    expect(snapshot.items[0].conflictsWith).toEqual([]);
    expect(snapshot.items[0].sourceSignalIds).toEqual([]);
    expect(snapshot.items[0].replay).toBeUndefined();
  });
});
