import { expect, it } from "vitest";
import { toUserError } from "./errorPresentation";

it("keeps actionable short errors while removing local paths", () => {
  expect(toUserError(new Error("cannot read C:\\Users\\alice\\repo\\file.ts"))).toBe("cannot read [local path]");
});

it("redacts credential-like values and bounds verbose failures", () => {
  const message = `api_key=secret ${"x".repeat(400)}`;
  const result = toUserError(message);
  expect(result).toContain("api_key=[redacted]");
  expect(result.length).toBeLessThanOrEqual(280);
});
