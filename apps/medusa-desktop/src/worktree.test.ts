import { describe, expect, it } from "vitest";

import { parsePorcelainV2 } from "./worktree";

describe("worktree porcelain parser", () => {
  it("captures branch tracking and every dirty-worktree class", () => {
    const state = parsePorcelainV2([
      "# branch.oid abc123",
      "# branch.head feature/workflow",
      "# branch.upstream origin/feature/workflow",
      "# branch.ab +2 -1",
      "1 M. N... 100644 100644 100644 abc abc src/staged.ts",
      "1 .M N... 100644 100644 100644 abc abc src/unstaged file.ts",
      "2 R. N... 100644 100644 100644 abc abc R100 src/new.ts\tsrc/old.ts",
      "u UU N... 100644 100644 100644 100644 abc abc abc src/conflicted.ts",
      "? src/untracked.ts",
      "! dist/generated.js",
    ].join("\n"));

    expect(state.branch).toBe("feature/workflow");
    expect(state.upstream).toBe("origin/feature/workflow");
    expect(state.ahead).toBe(2);
    expect(state.behind).toBe(1);
    expect(state.staged).toEqual(["src/conflicted.ts", "src/new.ts", "src/staged.ts"]);
    expect(state.unstaged).toEqual(["src/conflicted.ts", "src/unstaged file.ts"]);
    expect(state.untracked).toEqual(["src/untracked.ts"]);
    expect(state.conflicted).toEqual(["src/conflicted.ts"]);
    expect(state.ignored).toEqual(["dist/generated.js"]);
    expect(state.dirty).toBe(true);
    expect(state.entries.find((entry) => entry.kind === "renamed")).toMatchObject({
      path: "src/new.ts",
      originalPath: "src/old.ts",
    });
  });

  it("treats a clean detached worktree as clean", () => {
    const state = parsePorcelainV2("# branch.head (detached)\n# branch.ab +0 -0\n");

    expect(state.branch).toBeUndefined();
    expect(state.entries).toEqual([]);
    expect(state.dirty).toBe(false);
  });

  it("does not count ignored files as dirty", () => {
    const state = parsePorcelainV2("! target/cache.bin\n");

    expect(state.ignored).toEqual(["target/cache.bin"]);
    expect(state.dirty).toBe(false);
  });
});
