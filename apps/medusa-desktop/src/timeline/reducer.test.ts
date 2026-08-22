import { describe, expect, it } from "vitest";
import type { RuntimeEvent } from "../runtime";
import { emptyTimelineSnapshot } from "./model";
import { reduceTimelineEvent, reduceTimelineEvents } from "./reducer";

describe("structured timeline reducer", () => {
  it("projects assistant text and lifecycle state", () => {
    const snapshot = reduceTimelineEvents(emptyTimelineSnapshot, [
      { type: "started" },
      { type: "assistantText", text: "I am inspecting the failure." },
      { type: "turnFinished" },
    ]);

    expect(snapshot.busy).toBe(false);
    expect(snapshot.events).toHaveLength(1);
    expect(snapshot.events[0]).toMatchObject({
      kind: "message",
      status: "succeeded",
      title: "Medusa",
    });
  });

  it("does not project provider-private thinking tags", () => {
    const snapshot = reduceTimelineEvents(emptyTimelineSnapshot, [
      { type: "assistantText", text: "<think>private reasoning</think>Hello there" },
      { type: "assistantText", text: "<think>only reasoning</think>" },
    ]);

    expect(snapshot.events).toHaveLength(1);
    expect(snapshot.events[0]).toMatchObject({ text: "Hello there" });
  });

  it("updates activities in place while preserving sequence", () => {
    const started: RuntimeEvent = {
      type: "activity",
      activity: {
        id: "tests",
        kind: "tool",
        title: "Running tests",
        details: ["63 / 87 passed"],
      },
    };
    const completed: RuntimeEvent = {
      type: "activity",
      activity: {
        id: "tests",
        kind: "done",
        title: "Tests passed",
        details: ["87 passed"],
      },
    };

    const first = reduceTimelineEvent(emptyTimelineSnapshot, started);
    const second = reduceTimelineEvent(first, completed);

    expect(second.events).toHaveLength(1);
    expect(second.events[0]).toMatchObject({
      id: "activity-tests",
      sequence: 1,
      status: "succeeded",
      title: "Tests passed",
    });
    expect(second.nextSequence).toBe(2);
  });

  it("marks failures and questions as requiring attention", () => {
    const snapshot = reduceTimelineEvents(emptyTimelineSnapshot, [
      { type: "failed", message: "two tests failed" },
      {
        type: "question",
        prompts: [{
          header: "Approval",
          question: "Run the workspace tests?",
          options: [],
          multiSelect: false,
        }],
      },
    ]);

    expect(snapshot.events.map((event) => event.attention)).toEqual(["required", "required"]);
    expect(snapshot.events.map((event) => event.status)).toEqual(["failed", "blocked"]);
  });

  it("resets timeline content for a new session", () => {
    const populated = reduceTimelineEvent(emptyTimelineSnapshot, {
      type: "assistantText",
      text: "Existing message",
    });
    const reset = reduceTimelineEvent(populated, { type: "newSession" });

    expect(reset.events).toEqual([]);
    expect(reset.plan).toEqual([]);
    expect(reset.busy).toBe(false);
    expect(reset.revision).toBe(populated.revision + 1);
  });
});
