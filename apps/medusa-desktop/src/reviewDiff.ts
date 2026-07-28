export const MAX_DIFF_BYTES = 512_000;
export const MAX_DIFF_LINES = 12_000;

export type DiffLanguage =
  | "bash"
  | "css"
  | "go"
  | "html"
  | "javascript"
  | "json"
  | "markdown"
  | "python"
  | "rust"
  | "toml"
  | "typescript"
  | "yaml"
  | "text";

export type DiffLineKind = "context" | "addition" | "deletion" | "meta";

export interface DiffCell {
  lineNumber?: number;
  text: string;
  kind: DiffLineKind;
}

export interface DiffRow {
  id: string;
  before: DiffCell;
  after: DiffCell;
}

export interface ParsedReviewDiff {
  language: DiffLanguage;
  rows: DiffRow[];
  oversized: boolean;
  malformed: boolean;
  omittedLines: number;
}

const languageByExtension: Record<string, DiffLanguage> = {
  bash: "bash",
  css: "css",
  go: "go",
  htm: "html",
  html: "html",
  js: "javascript",
  json: "json",
  jsx: "javascript",
  md: "markdown",
  mjs: "javascript",
  py: "python",
  rs: "rust",
  sh: "bash",
  toml: "toml",
  ts: "typescript",
  tsx: "typescript",
  yaml: "yaml",
  yml: "yaml",
};

export function detectDiffLanguage(path: string): DiffLanguage {
  const filename = path.split("/").pop()?.toLowerCase() ?? "";
  if (filename === "dockerfile") return "bash";
  if (filename === "cargo.lock") return "toml";
  const extension = filename.includes(".") ? filename.split(".").pop() ?? "" : "";
  return languageByExtension[extension] ?? "text";
}

function emptyCell(): DiffCell {
  return { text: "", kind: "context" };
}

function parseHunkHeader(line: string): { before: number; after: number } | undefined {
  const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
  if (!match) return undefined;
  return { before: Number(match[1]), after: Number(match[2]) };
}

function pairChanges(
  deletions: DiffCell[],
  additions: DiffCell[],
  rows: DiffRow[],
  rowIndex: { value: number },
) {
  const count = Math.max(deletions.length, additions.length);
  for (let index = 0; index < count; index += 1) {
    rows.push({
      id: `row-${rowIndex.value++}`,
      before: deletions[index] ?? emptyCell(),
      after: additions[index] ?? emptyCell(),
    });
  }
}

export function parseReviewDiff(path: string, patch: string): ParsedReviewDiff {
  const byteLength = new TextEncoder().encode(patch).byteLength;
  const sourceLines = patch.split("\n");
  const oversized = byteLength > MAX_DIFF_BYTES || sourceLines.length > MAX_DIFF_LINES;
  const lines = oversized ? sourceLines.slice(0, MAX_DIFF_LINES) : sourceLines;
  const omittedLines = sourceLines.length - lines.length;
  const rows: DiffRow[] = [];
  const rowIndex = { value: 0 };
  let beforeLine = 0;
  let afterLine = 0;
  let malformed = false;
  let deletions: DiffCell[] = [];
  let additions: DiffCell[] = [];

  const flushChanges = () => {
    pairChanges(deletions, additions, rows, rowIndex);
    deletions = [];
    additions = [];
  };

  for (const line of lines) {
    const header = parseHunkHeader(line);
    if (header) {
      flushChanges();
      beforeLine = header.before;
      afterLine = header.after;
      rows.push({
        id: `row-${rowIndex.value++}`,
        before: { text: line, kind: "meta" },
        after: { text: line, kind: "meta" },
      });
      continue;
    }

    if (line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("---") || line.startsWith("+++")) {
      flushChanges();
      rows.push({
        id: `row-${rowIndex.value++}`,
        before: { text: line, kind: "meta" },
        after: { text: line, kind: "meta" },
      });
      continue;
    }

    if (line.startsWith("-") && !line.startsWith("---")) {
      deletions.push({ lineNumber: beforeLine++, text: line.slice(1), kind: "deletion" });
      continue;
    }

    if (line.startsWith("+") && !line.startsWith("+++")) {
      additions.push({ lineNumber: afterLine++, text: line.slice(1), kind: "addition" });
      continue;
    }

    flushChanges();
    if (line.startsWith(" ")) {
      rows.push({
        id: `row-${rowIndex.value++}`,
        before: { lineNumber: beforeLine++, text: line.slice(1), kind: "context" },
        after: { lineNumber: afterLine++, text: line.slice(1), kind: "context" },
      });
    } else if (line === "\\ No newline at end of file" || line === "") {
      rows.push({
        id: `row-${rowIndex.value++}`,
        before: { text: line, kind: "meta" },
        after: { text: line, kind: "meta" },
      });
    } else {
      malformed = true;
      rows.push({
        id: `row-${rowIndex.value++}`,
        before: { text: line, kind: "meta" },
        after: { text: line, kind: "meta" },
      });
    }
  }
  flushChanges();

  return {
    language: detectDiffLanguage(path),
    rows,
    oversized,
    malformed,
    omittedLines,
  };
}

export function evidenceFreshness(
  verification: "Verified" | "Failed" | "Stale" | "Unverified",
): "current" | "failed" | "stale" | "unavailable" {
  if (verification === "Verified") return "current";
  if (verification === "Failed") return "failed";
  if (verification === "Stale") return "stale";
  return "unavailable";
}

export function canPresentVerifiedCompletion(completion: {
  all_required_changes_reviewed: boolean;
  verification_current: boolean;
}): boolean {
  return completion.all_required_changes_reviewed && completion.verification_current;
}
