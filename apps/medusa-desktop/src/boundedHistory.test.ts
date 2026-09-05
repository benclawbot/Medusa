import { expect, it } from "vitest";
import { appendBounded, updateBoundedById } from "./boundedHistory";

it("keeps the newest entries within the configured history limit", () => {
  expect(appendBounded([1, 2], 3, 2)).toEqual([2, 3]);
});

it("updates an existing keyed entry without growing the history", () => {
  expect(updateBoundedById(
    [{ id: "a", value: 1 }, { id: "b", value: 2 }],
    { id: "a", value: 3 },
    2,
  )).toEqual([{ id: "a", value: 3 }, { id: "b", value: 2 }]);
});

it("appends a new keyed entry and drops the oldest item", () => {
  expect(updateBoundedById(
    [{ id: "a", value: 1 }, { id: "b", value: 2 }],
    { id: "c", value: 3 },
    2,
  )).toEqual([{ id: "b", value: 2 }, { id: "c", value: 3 }]);
});
