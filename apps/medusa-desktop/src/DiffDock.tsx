import { FileCode2, GitCompareArrows, RefreshCw, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { readRuntimeDiff, type DiffFile, type RepositoryDiff } from "./diff-runtime";

function pathFor(file: DiffFile): string {
  return file.status === "deleted" ? file.oldPath : file.newPath;
}

export function DiffDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");
  const [diff, setDiff] = useState<RepositoryDiff>({ files: [], additions: 0, deletions: 0 });
  const [selectedPath, setSelectedPath] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();

  const refresh = useCallback(async () => {
    const currentRepo = window.localStorage.getItem("medusa.desktop.repo") ?? "";
    setRepo(currentRepo);
    if (!currentRepo) return;
    setLoading(true);
    setError(undefined);
    try {
      const next = await readRuntimeDiff(currentRepo);
      setDiff(next);
      setSelectedPath((current) =>
        next.files.some((file) => pathFor(file) === current)
          ? current
          : next.files[0]
            ? pathFor(next.files[0])
            : "",
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

  const selected = useMemo(
    () => diff.files.find((file) => pathFor(file) === selectedPath),
    [diff.files, selectedPath],
  );

  return (
    <>
      <button className="diff-dock-trigger" onClick={() => setOpen(true)} aria-label="Open changes viewer">
        <GitCompareArrows size={16} /> Changes
      </button>
      {open && (
        <section className="diff-dock" aria-label="Repository changes">
          <header className="diff-dock-toolbar">
            <div>
              <h2>Working tree changes</h2>
              <p>{diff.files.length} files <span className="diff-add">+{diff.additions}</span> <span className="diff-del">−{diff.deletions}</span></p>
            </div>
            <div>
              <button onClick={() => void refresh()} disabled={loading}><RefreshCw size={15} /> {loading ? "Refreshing" : "Refresh"}</button>
              <button onClick={() => setOpen(false)} aria-label="Close changes viewer"><X size={16} /></button>
            </div>
          </header>
          {!repo && <div className="diff-empty">Open a repository to inspect working-tree changes.</div>}
          {error && <div className="diff-error">{error}</div>}
          {repo && !diff.files.length && !loading && <div className="diff-empty">No tracked changes against HEAD.</div>}
          {!!diff.files.length && (
            <div className="diff-layout">
              <nav className="diff-files" aria-label="Changed files">
                {diff.files.map((file) => {
                  const path = pathFor(file);
                  return (
                    <button key={`${file.oldPath}-${file.newPath}`} className={path === selectedPath ? "active" : ""} onClick={() => setSelectedPath(path)}>
                      <FileCode2 size={14} />
                      <span><strong>{path}</strong><small>{file.status}</small></span>
                      <em className="diff-add">+{file.additions}</em><em className="diff-del">−{file.deletions}</em>
                    </button>
                  );
                })}
              </nav>
              <section className="diff-file" aria-live="polite">
                {selected && (
                  <>
                    <header><strong>{pathFor(selected)}</strong>{selected.status === "renamed" && <small>{selected.oldPath} → {selected.newPath}</small>}</header>
                    {selected.binary ? <div className="diff-empty">Binary file changed. Text preview is unavailable.</div> : selected.hunks.map((hunk, hunkIndex) => (
                      <div className="diff-hunk" key={`${hunk.header}-${hunkIndex}`}>
                        <div className="diff-hunk-header">{hunk.header}</div>
                        {hunk.lines.map((line, lineIndex) => (
                          <div className={`diff-line ${line.kind}`} key={`${hunkIndex}-${lineIndex}`}>
                            <span>{line.oldLine ?? ""}</span><span>{line.newLine ?? ""}</span>
                            <code>{line.kind === "addition" ? "+" : line.kind === "deletion" ? "−" : " "}{line.text}</code>
                          </div>
                        ))}
                      </div>
                    ))}
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
