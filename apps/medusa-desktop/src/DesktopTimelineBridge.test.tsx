import { describe, expect, it } from "vitest";
import { summarizePlan } from "./DesktopTimelineBridge";
import type { PlanStep } from "./runtime";

describe("summarizePlan", () => {
  it("reports DeepSeek-style completed, in-progress, and pending counts", () => {
    const plan: PlanStep[] = [
      { title: "Read seam contract", status: "completed" },
      { title: "Read composition wiring", status: "completed" },
      { title: "Design provider", status: "completed" },
      { title: "Scaffold package", status: "inProgress" },
      { title: "Write tests", status: "pending" },
      { title: "Document assumptions", status: "pending" },
      { title: "Run typecheck", status: "pending" },
    ];

    expect(summarizePlan(plan)).toEqual({
      completed: 3,
      inProgress: 1,
      pending: 3,
    });
  });

  it("keeps failed steps in the outstanding count so totals remain stable", () => {
    const plan: PlanStep[] = [
      { title: "Done", status: "completed" },
      { title: "Failed verification", status: "failed" },
      { title: "Queued follow-up", status: "pending" },
    ];

    expect(summarizePlan(plan)).toEqual({
      completed: 1,
      inProgress: 0,
      pending: 2,
    });
  });
});
