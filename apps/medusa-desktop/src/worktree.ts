export type WorktreeEntryKind = "ordinary" | "renamed" | "unmerged" | "untracked" | "ignored";

export interface WorktreeEntry {
  path: string;
  originalPath?: string;
  kind: WorktreeEntryKind;
  indexStatus: string;
  worktreeStatus: string;
}

export interface WorktreeState {
  branch?: string;
  upstream?: string;
  ahead: number;
  behind: number;
  entries: WorktreeEntry[];
  staged: string[];
  unstaged: string[];
  untracked: string[];
  conflicted: string[];
  ignored: string[];
  dirty: boolean;
}

function uniqueSorted(values: string[]): string[] {
  return [...new Set(values)].sort((left, right) => left.localeCompare(right));
}

export function parsePorcelainV2(output: string): WorktreeState {
  let branch: string | undefined;
  let upstream: string | undefined;
  let ahead = 0;
  let behind = 0;
  const entries: WorktreeEntry[] = [];

  for (const rawLine of output.split("\n")) {
    const line = rawLine.trimEnd();
    if (!line) continue;

    if (line.startsWith("# branch.head ")) {
      const value = line.slice("# branch.head ".length);
      branch = value === "(detached)" ? undefined : value;
      continue;
    }
    if (line.startsWith("# branch.upstream ")) {
      upstream = line.slice("# branch.upstream ".length);
      continue;
    }
    if (line.startsWith("# branch.ab ")) {
      const match = /^# branch\.ab \+(\d+) -(\d+)$/.exec(line);
      if (match) {
        ahead = Number(match[1]);
        behind = Number(match[2]);
      }
      continue;
    }

    if (line.startsWith("? ")) {
      entries.push({ path: line.slice(2), kind: "untracked", indexStatus: "?", worktreeStatus: "?" });
      continue;
    }
    if (line.startsWith("! ")) {
      entries.push({ path: line.slice(2), kind: "ignored", indexStatus: "!", worktreeStatus: "!" });
      continue;
    }

    const fields = line.split(" ");
    const recordType = fields[0];
    const status = fields[1] ?? "..";
    if (recordType === "1" && fields.length >= 9) {
      entries.push({
        path: fields.slice(8).join(" "),
        kind: "ordinary",
        indexStatus: status[0] ?? ".",
        worktreeStatus: status[1] ?? ".",
      });
      continue;
    }
    if (recordType === "2" && fields.length >= 10) {
      const pathAndOriginal = fields.slice(9).join(" ").split("\t");
      entries.push({
        path: pathAndOriginal[0],
        originalPath: pathAndOriginal[1],
        kind: "renamed",
        indexStatus: status[0] ?? ".",
        worktreeStatus: status[1] ?? ".",
      });
      continue;
    }
    if (recordType === "u" && fields.length >= 11) {
      entries.push({
        path: fields.slice(10).join(" "),
        kind: "unmerged",
        indexStatus: status[0] ?? "U",
        worktreeStatus: status[1] ?? "U",
      });
    }
  }

  const staged = uniqueSorted(
    entries
      .filter((entry) => entry.kind !== "untracked" && entry.kind !== "ignored" && entry.indexStatus !== ".")
      .map((entry) => entry.path),
  );
  const unstaged = uniqueSorted(
    entries
      .filter((entry) => entry.kind !== "untracked" && entry.kind !== "ignored" && entry.worktreeStatus !== ".")
      .map((entry) => entry.path),
  );
  const untracked = uniqueSorted(entries.filter((entry) => entry.kind === "untracked").map((entry) => entry.path));
  const conflicted = uniqueSorted(entries.filter((entry) => entry.kind === "unmerged").map((entry) => entry.path));
  const ignored = uniqueSorted(entries.filter((entry) => entry.kind === "ignored").map((entry) => entry.path));

  return {
    branch,
    upstream,
    ahead,
    behind,
    entries,
    staged,
    unstaged,
    untracked,
    conflicted,
    ignored,
    dirty: staged.length > 0 || unstaged.length > 0 || untracked.length > 0 || conflicted.length > 0,
  };
}
