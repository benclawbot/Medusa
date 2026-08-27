import {
  AlertTriangle,
  Check,
  Download,
  FileCode2,
  Filter,
  GitCompareArrows,
  Link2,
  RefreshCw,
  RotateCcw,
  X,
} from "lucide-react";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  applyReviewAction,
  exportReviewAudit,
  readReview,
  type ReviewFileModel,
  type ReviewProvenance,
  type ReviewWorkspace,
} from "./reviewApi";
import { useDesktopToolRequest } from "./desktop-tools";
import {
  canPresentVerifiedCompletion,
  evidenceFreshness,
  parseReviewDiff,
  type DiffCell,
  type DiffLanguage,
} from "./reviewDiff";
import { useDialogFocus } from "./useDialogFocus";

const empty: ReviewWorkspace = {
  snapshot: {
    id: "",
    repository_fingerprint: "",
    created_at_unix_ms: 0,
    files: [],
  },
  files: [],
  completion: {
    unreviewed_paths: [],
    stale_or_failed_paths: [],
    all_required_changes_reviewed: true,
    verification_current: true,
  },
};

const keywords: Partial<Record<DiffLanguage, Set<string>>> = {
  rust: new Set([
    "as",
    "async",
    "await",
    "const",
    "crate",
    "enum",
    "fn",
    "impl",
    "let",
    "match",
    "mod",
    "mut",
    "pub",
    "return",
    "self",
    "struct",
    "trait",
    "use",
    "where",
  ]),
  typescript: new Set([
    "async",
    "await",
    "class",
    "const",
    "export",
    "extends",
    "function",
    "if",
    "import",
    "interface",
    "let",
    "new",
    "return",
    "type",
  ]),
  javascript: new Set([
    "async",
    "await",
    "class",
    "const",
    "export",
    "function",
    "if",
    "import",
    "let",
    "new",
    "return",
  ]),
  python: new Set([
    "async",
    "await",
    "class",
    "def",
    "elif",
    "else",
    "for",
    "from",
    "if",
    "import",
    "in",
    "return",
    "while",
  ]),
  go: new Set([
    "const",
    "defer",
    "else",
    "for",
    "func",
    "go",
    "if",
    "import",
    "interface",
    "package",
    "return",
    "struct",
    "type",
    "var",
  ]),
};

function syntaxLine(text: string, language: DiffLanguage): ReactNode {
  if (language === "text" || language === "markdown") return text;
  const words = keywords[language];
  const parts = text.split(/("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\b[A-Za-z_][A-Za-z0-9_]*\b|\b\d+(?:\.\d+)?\b)/g);
  return parts.map((part, index) => {
    if (/^("|'|`)/.test(part)) {
      return <span className="syntax-string" key={`${index}-${part}`}>{part}</span>;
    }
    if (/^\d/.test(part)) {
      return <span className="syntax-number" key={`${index}-${part}`}>{part}</span>;
    }
    if (words?.has(part)) {
      return <span className="syntax-keyword" key={`${index}-${part}`}>{part}</span>;
    }
    return part;
  });
}

function DiffCellView({
  cell,
  language,
  side,
}: {
  cell: DiffCell;
  language: DiffLanguage;
  side: "before" | "after";
}) {
  return (
    <div className={`review-diff-cell review-${cell.kind}`} data-side={side}>
      <span className="review-line-number" aria-hidden="true">
        {cell.lineNumber ?? ""}
      </span>
      <code>{syntaxLine(cell.text, language)}</code>
    </div>
  );
}

function ProvenancePanel({
  provenance,
  verification,
  label,
}: {
  provenance: ReviewProvenance;
  verification: ReviewFileModel["verification"];
  label: string;
}) {
  const freshness = evidenceFreshness(verification);
  const hasReferences = Boolean(
    provenance.task_step_id ||
      provenance.tool_execution_id ||
      provenance.rationale ||
      provenance.verification_event_ids.length,
  );
  return (
    <aside className="review-evidence" aria-label={`${label} evidence`}>
      <div className="review-evidence-heading">
        <strong>Authoritative evidence</strong>
        <span className={`review-evidence-status status-${freshness}`}>
          {freshness}
        </span>
      </div>
      {!hasReferences ? (
        <p>Evidence unavailable. The desktop does not infer missing provenance.</p>
      ) : (
        <dl>
          {provenance.task_step_id && (
            <div>
              <dt>Task step</dt>
              <dd><Link2 size={12} /> {provenance.task_step_id}</dd>
            </div>
          )}
          {provenance.tool_execution_id && (
            <div>
              <dt>Tool activity</dt>
              <dd><Link2 size={12} /> {provenance.tool_execution_id}</dd>
            </div>
          )}
          {provenance.rationale && (
            <div>
              <dt>Rationale</dt>
              <dd>{provenance.rationale}</dd>
            </div>
          )}
          <div>
            <dt>Verification events</dt>
            <dd>
              {provenance.verification_event_ids.length
                ? provenance.verification_event_ids.join(", ")
                : "Unavailable"}
            </dd>
          </div>
        </dl>
      )}
      {(freshness === "failed" || freshness === "stale") && (
        <p className="review-evidence-warning" role="alert">
          <AlertTriangle size={13} />
          {freshness === "failed"
            ? "Verification failed. Completion remains blocked."
            : "Verification predates the latest file version. Refresh and rerun checks."}
        </p>
      )}
    </aside>
  );
}

export function DiffDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(
    () => window.localStorage.getItem("medusa.desktop.repo") ?? "",
  );
  const [review, setReview] = useState<ReviewWorkspace>(empty);
  const [selectedPath, setSelectedPath] = useState("");
  const [selectedHunkId, setSelectedHunkId] = useState<string>();
  const [filter, setFilter] = useState("all");
  const [layout, setLayout] = useState<"unified" | "side-by-side">("unified");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const dialogRef = useRef<HTMLElement>(null);
  const close = useCallback(() => setOpen(false), []);
  const openFromRail = useCallback(() => setOpen(true), []);
  useDesktopToolRequest("review", openFromRail);
  useDialogFocus(open, dialogRef, close);

  const refresh = useCallback(async () => {
    const currentRepo = window.localStorage.getItem("medusa.desktop.repo") ?? "";
    setRepo(currentRepo);
    if (!currentRepo) return;
    setLoading(true);
    setError(undefined);
    try {
      const next = await readReview(currentRepo);
      setReview(next);
      setSelectedPath((current) =>
        next.snapshot.files.some((file) => file.path === current)
          ? current
          : next.snapshot.files[0]?.path ?? "",
      );
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  const visible = useMemo(
    () =>
      review.snapshot.files.filter((file) => {
        if (filter === "all") return true;
        if (filter === "unreviewed") return file.review_state === "Unreviewed";
        if (filter === "generated") return file.origin === "Generated";
        if (filter === "unverified") return file.verification !== "Verified";
        if (filter === "policy") return file.policy_sensitive;
        return file.kind.toLowerCase() === filter;
      }),
    [review.snapshot.files, filter],
  );

  const selectedModel = review.snapshot.files.find(
    (file) => file.path === selectedPath,
  );
  const selectedDiff = review.files.find((file) => file.path === selectedPath);
  const parsedDiff = useMemo(
    () => parseReviewDiff(selectedPath, selectedDiff?.patch ?? ""),
    [selectedDiff?.patch, selectedPath],
  );
  const selectedHunk = selectedModel?.hunks.find((hunk) => hunk.id === selectedHunkId);
  const revertSafe =
    selectedModel?.hunks.every(
      (hunk) => !hunk.ambiguous && !hunk.overlaps_later_edits,
    ) ?? false;
  const verifiedCompletion = canPresentVerifiedCompletion(review.completion);

  const act = useCallback(
    async (
      operation: "accept-file" | "revert-file" | "revert-hunk" | "accept-task",
      file?: ReviewFileModel,
      hunkId?: string,
    ) => {
      if (!repo) return;
      setLoading(true);
      setError(undefined);
      try {
        const hunk = file?.hunks.find((item) => item.id === hunkId);
        const next = await applyReviewAction(repo, {
          operation,
          path: file?.path,
          hunkId,
          snapshotId: review.snapshot.id,
          fileFingerprint: file?.current_fingerprint,
          hunkFingerprint: hunk?.current_fingerprint,
        });
        setReview(next);
      } catch (cause) {
        setError(String(cause));
      } finally {
        setLoading(false);
      }
    },
    [repo, review.snapshot.id],
  );

  const downloadAudit = useCallback(async () => {
    if (!repo) return;
    try {
      const audit = await exportReviewAudit(repo);
      const blob = new Blob([JSON.stringify(audit, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = "medusa-review-audit.json";
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (cause) {
      setError(String(cause));
    }
  }, [repo]);

  return (
    <>
      {open && (
        <div className="diff-dock-backdrop">
          <section
            ref={dialogRef}
            className="diff-dock"
            role="dialog"
            aria-modal="true"
            aria-labelledby="diff-dialog-title"
            tabIndex={-1}
          >
          <header className="diff-dock-toolbar">
            <div>
              <h2 id="diff-dialog-title">Code review</h2>
              <p aria-live="polite">
                {review.snapshot.files.length} files · {review.completion.unreviewed_paths.length} unreviewed · {review.completion.stale_or_failed_paths.length} stale/failed
              </p>
            </div>
            <div>
              <button onClick={() => void refresh()} disabled={loading}>
                <RefreshCw size={15} /> Refresh
              </button>
              <button onClick={() => void downloadAudit()}>
                <Download size={15} /> Audit
              </button>
              <button onClick={close} aria-label="Close code review"><X size={16} /></button>
            </div>
          </header>
          <div className="review-controls">
            <Filter size={14} />
            <select value={filter} onChange={(event) => setFilter(event.target.value)} aria-label="Review filter">
              <option value="all">All changes</option>
              <option value="unreviewed">Unreviewed</option>
              <option value="modified">Modified</option>
              <option value="added">Added</option>
              <option value="deleted">Deleted</option>
              <option value="renamed">Renamed</option>
              <option value="generated">Generated</option>
              <option value="unverified">Unverified</option>
              <option value="policy">Policy-sensitive</option>
            </select>
            <button onClick={() => setLayout((value) => value === "unified" ? "side-by-side" : "unified")} aria-label="Toggle diff layout">
              {layout}
            </button>
            <button disabled={!verifiedCompletion || loading} onClick={() => void act("accept-task")} title={verifiedCompletion ? undefined : "Review and current verification are required before verified completion."}>
              <Check size={14} /> Accept task result
            </button>
          </div>
          {!verifiedCompletion && review.snapshot.files.length > 0 && (
            <div className="review-completion-block" role="status">
              <AlertTriangle size={14} /> Verified completion is blocked until every required change is reviewed and verification is current.
            </div>
          )}
          {!repo && <div className="diff-empty">Open a repository to review working-tree changes.</div>}
          {error && <div className="diff-error" role="alert">{error}</div>}
          {repo && !review.snapshot.files.length && !loading && <div className="diff-empty">No changes against HEAD.</div>}
          {!!review.snapshot.files.length && (
            <div className="diff-layout">
              <nav className="diff-files" aria-label="Changed files">
                {visible.map((file) => (
                  <button
                    key={file.path}
                    className={file.path === selectedPath ? "active" : ""}
                    aria-current={file.path === selectedPath ? "true" : undefined}
                    onClick={() => { setSelectedPath(file.path); setSelectedHunkId(undefined); }}
                  >
                    <FileCode2 size={14} />
                    <span>
                      <strong>{file.path}</strong>
                      <small>{file.kind} · {file.origin} · {file.verification} · {file.review_state}</small>
                    </span>
                  </button>
                ))}
              </nav>
              <section className={`diff-file review-${layout}`} aria-live="polite" aria-label="Selected file review">
                {selectedModel && selectedDiff && (
                  <>
                    <header>
                      <div>
                        <strong>{selectedModel.path}</strong>
                        {selectedModel.previous_path && <small>{selectedModel.previous_path} → {selectedModel.path}</small>}
                        <small>{selectedModel.policy_sensitive ? "Policy-sensitive · " : ""}{selectedModel.binary ? "Binary · " : ""}{selectedModel.origin} · {parsedDiff.language}</small>
                      </div>
                      <div>
                        <button disabled={selectedModel.origin !== "Medusa" || loading} onClick={() => void act("accept-file", selectedModel)}><Check size={14} /> Accept</button>
                        <button
                          disabled={selectedModel.origin !== "Medusa" || selectedModel.binary || selectedModel.kind === "Renamed" || !revertSafe || loading}
                          title={revertSafe ? undefined : "Revert is disabled because tracked-file provenance is ambiguous."}
                          onClick={() => void act("revert-file", selectedModel)}
                        ><RotateCcw size={14} /> Revert file</button>
                      </div>
                    </header>
                    <ProvenancePanel provenance={selectedHunk?.provenance ?? selectedModel.provenance} verification={selectedModel.verification} label={selectedHunk ? "Selected hunk" : "Selected file"} />
                    {selectedModel.binary ? (
                      <div className="diff-empty">Binary content is shown safely without a text preview. Selective revert is disabled.</div>
                    ) : (
                      <div className={`review-diff-grid layout-${layout}`} role="table" aria-label={`${parsedDiff.language} diff with line numbers`} tabIndex={0}>
                        {parsedDiff.oversized && (
                          <div className="review-diff-notice" role="status">Large diff preview is bounded. {parsedDiff.omittedLines} lines were omitted.</div>
                        )}
                        {parsedDiff.malformed && (
                          <div className="review-diff-notice" role="status">Some malformed patch lines are displayed as inert metadata.</div>
                        )}
                        {parsedDiff.rows.map((row) =>
                          layout === "side-by-side" ? (
                            <div className="review-diff-row" role="row" key={row.id}>
                              <DiffCellView cell={row.before} language={parsedDiff.language} side="before" />
                              <DiffCellView cell={row.after} language={parsedDiff.language} side="after" />
                            </div>
                          ) : (
                            <div className="review-diff-row unified-row" role="row" key={row.id}>
                              {row.before.kind === "deletion" && <DiffCellView cell={row.before} language={parsedDiff.language} side="before" />}
                              {row.after.kind === "addition" && <DiffCellView cell={row.after} language={parsedDiff.language} side="after" />}
                              {row.before.kind !== "deletion" && row.after.kind !== "addition" && <DiffCellView cell={row.after} language={parsedDiff.language} side="after" />}
                            </div>
                          ),
                        )}
                      </div>
                    )}
                    {!selectedModel.binary && selectedDiff.hunks.map((hunk) => {
                      const model = selectedModel.hunks.find((item) => item.id === hunk.id);
                      return (
                        <div className={`review-hunk-actions ${selectedHunkId === hunk.id ? "active" : ""}`} key={hunk.id}>
                          <button className="review-hunk-link" onClick={() => setSelectedHunkId(hunk.id)} aria-label={`Show evidence for ${hunk.header}`}><Link2 size={12} /> <code>{hunk.header}</code></button>
                          <button
                            disabled={selectedModel.origin !== "Medusa" || selectedModel.kind === "Renamed" || model?.ambiguous || model?.overlaps_later_edits || loading}
                            title={model?.ambiguous || model?.overlaps_later_edits ? "Hunk revert is disabled because write provenance is ambiguous." : undefined}
                            onClick={() => void act("revert-hunk", selectedModel, hunk.id)}
                          ><RotateCcw size={13} /> Revert hunk</button>
                        </div>
                      );
                    })}
                    <footer className="review-note">Accepting review state does not commit, push, or merge. Refresh is required after working-tree drift.</footer>
                  </>
                )}
              </section>
            </div>
          )}
          </section>
        </div>
      )}
    </>
  );
}
