import {
  Check,
  Download,
  FileCode2,
  Filter,
  GitCompareArrows,
  RefreshCw,
  RotateCcw,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  applyReviewAction,
  exportReviewAudit,
  readReview,
  type ReviewFileModel,
  type ReviewWorkspace,
} from "./reviewApi";
import { useDialogFocus } from "./useDialogFocus";

const empty: ReviewWorkspace = {
  snapshot: { id: "", files: [] },
  files: [],
  completion: {
    unreviewed_paths: [],
    stale_or_failed_paths: [],
    all_required_changes_reviewed: true,
    verification_current: true,
  },
};

function splitPatch(patch: string): { before: string; after: string } {
  const before: string[] = [];
  const after: string[] = [];
  for (const line of patch.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      before.push(line);
      after.push(line);
    } else if (line.startsWith("+")) {
      before.push("");
      after.push(line);
    } else if (line.startsWith("-")) {
      before.push(line);
      after.push("");
    } else {
      before.push(line);
      after.push(line);
    }
  }
  return { before: before.join("\n"), after: after.join("\n") };
}

export function DiffDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(
    () => window.localStorage.getItem("medusa.desktop.repo") ?? "",
  );
  const [review, setReview] = useState<ReviewWorkspace>(empty);
  const [selectedPath, setSelectedPath] = useState("");
  const [filter, setFilter] = useState("all");
  const [layout, setLayout] = useState<"unified" | "side-by-side">("unified");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const dialogRef = useRef<HTMLElement>(null);
  const close = useCallback(() => setOpen(false), []);
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
  const splitSelectedPatch = useMemo(
    () => splitPatch(selectedDiff?.patch ?? ""),
    [selectedDiff?.patch],
  );
  const revertSafe =
    selectedModel?.hunks.every(
      (hunk) => !hunk.ambiguous && !hunk.overlaps_later_edits,
    ) ?? false;

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
      <button
        className="diff-dock-trigger"
        onClick={() => setOpen(true)}
        aria-label="Open code review"
        aria-haspopup="dialog"
        aria-expanded={open}
      >
        <GitCompareArrows size={16} /> Review
      </button>
      {open && (
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
                {review.snapshot.files.length} files ·{" "}
                {review.completion.unreviewed_paths.length} unreviewed ·{" "}
                {review.completion.stale_or_failed_paths.length} stale/failed
              </p>
            </div>
            <div>
              <button onClick={() => void refresh()} disabled={loading}>
                <RefreshCw size={15} /> Refresh
              </button>
              <button onClick={() => void downloadAudit()}>
                <Download size={15} /> Audit
              </button>
              <button onClick={close} aria-label="Close code review">
                <X size={16} />
              </button>
            </div>
          </header>
          <div className="review-controls">
            <Filter size={14} />
            <select
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              aria-label="Review filter"
            >
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
            <button
              onClick={() =>
                setLayout((value) =>
                  value === "unified" ? "side-by-side" : "unified",
                )
              }
            >
              {layout}
            </button>
            <button
              disabled={
                !review.completion.all_required_changes_reviewed ||
                !review.completion.verification_current ||
                loading
              }
              onClick={() => void act("accept-task")}
            >
              <Check size={14} /> Accept task result
            </button>
          </div>
          {!repo && (
            <div className="diff-empty">
              Open a repository to review working-tree changes.
            </div>
          )}
          {error && (
            <div className="diff-error" role="alert">
              {error}
            </div>
          )}
          {repo && !review.snapshot.files.length && !loading && (
            <div className="diff-empty">No changes against HEAD.</div>
          )}
          {!!review.snapshot.files.length && (
            <div className="diff-layout">
              <nav className="diff-files" aria-label="Changed files">
                {visible.map((file) => (
                  <button
                    key={file.path}
                    className={file.path === selectedPath ? "active" : ""}
                    aria-current={
                      file.path === selectedPath ? "true" : undefined
                    }
                    onClick={() => setSelectedPath(file.path)}
                  >
                    <FileCode2 size={14} />
                    <span>
                      <strong>{file.path}</strong>
                      <small>
                        {file.kind} · {file.origin} · {file.verification} ·{" "}
                        {file.review_state}
                      </small>
                    </span>
                  </button>
                ))}
              </nav>
              <section
                className={`diff-file review-${layout}`}
                aria-live="polite"
                aria-label="Selected file review"
              >
                {selectedModel && selectedDiff && (
                  <>
                    <header>
                      <div>
                        <strong>{selectedModel.path}</strong>
                        {selectedModel.previous_path && (
                          <small>
                            {selectedModel.previous_path} → {selectedModel.path}
                          </small>
                        )}
                        <small>
                          {selectedModel.policy_sensitive
                            ? "Policy-sensitive · "
                            : ""}
                          {selectedModel.binary ? "Binary · " : ""}
                          {selectedModel.origin}
                        </small>
                      </div>
                      <div>
                        <button
                          disabled={
                            selectedModel.origin !== "Medusa" || loading
                          }
                          onClick={() => void act("accept-file", selectedModel)}
                        >
                          <Check size={14} /> Accept
                        </button>
                        <button
                          disabled={
                            selectedModel.origin !== "Medusa" ||
                            selectedModel.binary ||
                            selectedModel.kind === "Renamed" ||
                            !revertSafe ||
                            loading
                          }
                          title={
                            revertSafe
                              ? undefined
                              : "Revert is disabled because tracked-file provenance is ambiguous."
                          }
                          onClick={() => void act("revert-file", selectedModel)}
                        >
                          <RotateCcw size={14} /> Revert file
                        </button>
                      </div>
                    </header>
                    {selectedModel.binary ? (
                      <div className="diff-empty">
                        Binary content is shown safely without a text preview.
                        Selective revert is disabled.
                      </div>
                    ) : layout === "side-by-side" ? (
                      <div
                        className="review-side-by-side"
                        style={{
                          display: "grid",
                          gridTemplateColumns: "minmax(0, 1fr) minmax(0, 1fr)",
                          gap: "1px",
                          overflow: "auto",
                        }}
                      >
                        <pre className="review-patch" aria-label="Before changes">
                          {splitSelectedPatch.before}
                        </pre>
                        <pre className="review-patch" aria-label="After changes">
                          {splitSelectedPatch.after}
                        </pre>
                      </div>
                    ) : (
                      <pre className="review-patch">{selectedDiff.patch}</pre>
                    )}
                    {!selectedModel.binary &&
                      selectedDiff.hunks.map((hunk) => (
                        <div className="review-hunk-actions" key={hunk.id}>
                          <code>{hunk.header}</code>
                          <button
                            disabled={
                              selectedModel.origin !== "Medusa" ||
                              selectedModel.kind === "Renamed" ||
                              hunk.ambiguous ||
                              hunk.overlaps_later_edits ||
                              loading
                            }
                            title={
                              hunk.ambiguous || hunk.overlaps_later_edits
                                ? "Hunk revert is disabled because write provenance is ambiguous."
                                : undefined
                            }
                            onClick={() =>
                              void act("revert-hunk", selectedModel, hunk.id)
                            }
                          >
                            <RotateCcw size={13} /> Revert hunk
                          </button>
                        </div>
                      ))}
                    <footer className="review-note">
                      Accepting review state does not commit, push, or merge.
                      Refresh is required after working-tree drift.
                    </footer>
                  </>
                )}
              </section>
            </div>
          )}
        </section>
      )}
    </>
  );
}
