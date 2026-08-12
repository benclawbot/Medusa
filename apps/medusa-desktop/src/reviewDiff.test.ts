import { describe, expect, it } from "vitest";
import {
  MAX_DIFF_LINES,
  canPresentVerifiedCompletion,
  detectDiffLanguage,
  evidenceFreshness,
  parseReviewDiff,
} from "./reviewDiff";

describe("review diff model", () => {
  it("detects supported languages without guessing unsupported files", () => {
    expect(detectDiffLanguage("src/main.rs")).toBe("rust");
    expect(detectDiffLanguage("web/view.tsx")).toBe("typescript");
    expect(detectDiffLanguage("Dockerfile")).toBe("bash");
    expect(detectDiffLanguage("assets/logo.bin")).toBe("text");
  });

  it("aligns replacements and preserves before and after line numbers", () => {
    const parsed = parseReviewDiff(
      "src/main.rs",
      "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,3 +10,3 @@\n context\n-old value\n+new value\n tail",
    );
    const replacement = parsed.rows.find(
      (row) => row.before.kind === "deletion" && row.after.kind === "addition",
    );
    expect(replacement?.before).toMatchObject({ lineNumber: 11, text: "old value" });
    expect(replacement?.after).toMatchObject({ lineNumber: 11, text: "new value" });
    expect(parsed.language).toBe("rust");
    expect(parsed.malformed).toBe(false);
  });

  it("aligns unmatched additions and deletions with empty cells", () => {
    const parsed = parseReviewDiff(
      "file.txt",
      "@@ -1,2 +1,3 @@\n-one\n-two\n+first\n+second\n+third",
    );
    const changed = parsed.rows.filter(
      (row) => row.before.kind === "deletion" || row.after.kind === "addition",
    );
    expect(changed).toHaveLength(3);
    expect(changed[2].before.lineNumber).toBeUndefined();
    expect(changed[2].after.text).toBe("third");
  });

  it("bounds oversized input", () => {
    const patch = ["@@ -1,1 +1,1 @@", ...Array.from({ length: MAX_DIFF_LINES + 20 }, () => " line")].join("\n");
    const parsed = parseReviewDiff("huge.txt", patch);
    expect(parsed.oversized).toBe(true);
    expect(parsed.omittedLines).toBeGreaterThan(0);
    expect(parsed.rows.length).toBeLessThanOrEqual(MAX_DIFF_LINES);
  });

  it("keeps repository text inert instead of interpreting markup", () => {
    const parsed = parseReviewDiff(
      "index.html",
      "@@ -1 +1 @@\n-<img src=x onerror=alert(1)>\n+<script>alert(1)</script>",
    );
    const row = parsed.rows.find((item) => item.after.kind === "addition");
    expect(row?.after.text).toBe("<script>alert(1)</script>");
  });

  it("distinguishes current, failed, stale, and unavailable evidence", () => {
    expect(evidenceFreshness("Verified")).toBe("current");
    expect(evidenceFreshness("Failed")).toBe("failed");
    expect(evidenceFreshness("Stale")).toBe("stale");
    expect(evidenceFreshness("Unverified")).toBe("unavailable");
  });

  it("blocks verified completion until review and verification are current", () => {
    expect(
      canPresentVerifiedCompletion({
        all_required_changes_reviewed: true,
        verification_current: true,
      }),
    ).toBe(true);
    expect(
      canPresentVerifiedCompletion({
        all_required_changes_reviewed: false,
        verification_current: true,
      }),
    ).toBe(false);
    expect(
      canPresentVerifiedCompletion({
        all_required_changes_reviewed: true,
        verification_current: false,
      }),
    ).toBe(false);
  });
});
