import { useEffect, useMemo, useState } from "react";
import { Download, RefreshCw, ShieldCheck, X } from "lucide-react";
import {
  exportLearningAudit,
  loadLearningReview,
  previewLearningExport,
  saveLearningPrivacy,
  transitionLearning,
  type LearningPrivacy,
  type LearningReviewItem,
  type LearningReviewSnapshot,
} from "./learningApi";
import { useDockShell } from "./useDockShell";
import "./learning-dock.css";

function actions(item: LearningReviewItem) {
  switch (item.state) {
    case "proposed": return ["approve", "defer", "reject"] as const;
    case "deferred": return ["approve", "reject"] as const;
    case "approved": return ["validate"] as const;
    case "validated": return ["activate"] as const;
    case "active": return ["suspend", "rollback"] as const;
    case "suspended": return ["activate", "rollback"] as const;
    default: return [] as const;
  }
}

export function LearningDock() {
  const [data, setData] = useState<LearningReviewSnapshot>();
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const repo = window.localStorage.getItem("medusa.desktop.repo") ?? "";
  const { open, setOpen, close, error, setError, dialogRef } = useDockShell<HTMLDivElement>("learning");

  const reload = async () => {
    if (!repo) return;
    setBusy(true);
    try {
      setData(await loadLearningReview(repo));
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (open) void reload();
  }, [open, repo]);

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return data?.items ?? [];
    return (data?.items ?? []).filter((item) =>
      [item.id, item.title, item.generalizedRule, item.rootCause, item.scope, item.kind, item.state]
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }, [data, filter]);

  const act = async (item: LearningReviewItem, action: Parameters<typeof transitionLearning>[2]) => {
    if (!data) return;
    setBusy(true);
    try {
      setData(await transitionLearning(repo, item.id, action, data.revision));
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
      await reload();
    } finally {
      setBusy(false);
    }
  };

  const updatePrivacy = async (key: keyof LearningPrivacy) => {
    if (!data) return;
    const privacy = { ...data.privacy, [key]: !data.privacy[key] };
    setBusy(true);
    try {
      setData(await saveLearningPrivacy(repo, privacy, data.revision));
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
      await reload();
    } finally {
      setBusy(false);
    }
  };

  const exportAudit = async () => {
    setBusy(true);
    try {
      const preview = await previewLearningExport(repo);
      if (!preview.safe) throw new Error(`Export blocked: ${preview.blockedFields.join(", ")}`);
      const exportValue = await exportLearningAudit(repo);
      await navigator.clipboard.writeText(JSON.stringify(exportValue, null, 2));
      setError("Audit export copied to clipboard after redaction checks.");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  return <>
    {open && <div className="learning-overlay">
      <div ref={dialogRef} className="learning-shell" role="dialog" aria-modal="true" aria-label="Learning review" tabIndex={-1}>
        <header>
          <div><span className="eyebrow">Authoritative lifecycle</span><h2>Learning review</h2></div>
          <div className="learning-header-actions">
            <button onClick={() => void reload()} disabled={busy}><RefreshCw size={15}/>Refresh</button>
            <button onClick={() => void exportAudit()} disabled={busy || !repo}><Download size={15}/>Export audit</button>
            <button aria-label="Close learning review" onClick={close}><X size={17}/></button>
          </div>
        </header>
        {!repo && <p className="learning-empty">Open a repository to review learned behavior.</p>}
        {error && <div className="learning-error" role="status">{error}</div>}
        {data && <>
          <section className="learning-privacy" aria-label="Learning privacy controls">
            <div><ShieldCheck size={18}/><div><strong>Private by default</strong><small>Raw microphone transcripts, image bytes, credentials, and unrelated source are excluded.</small></div></div>
            {Object.entries(data.privacy).map(([key, value]) => <label key={key}>
              <input type="checkbox" checked={value} onChange={() => void updatePrivacy(key as keyof LearningPrivacy)} disabled={busy}/>
              {key.replace(/[A-Z]/g, (letter) => ` ${letter.toLowerCase()}`)}
            </label>)}
          </section>
          <input className="learning-filter" value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="Filter by state, scope, kind, or text" aria-label="Filter learning items"/>
          <div className="learning-list">
            {visible.map((item) => <article key={item.id} tabIndex={0}>
              <div className="learning-title"><strong>{item.title}</strong><span>{item.state}</span></div>
              <dl>
                <div><dt>Learned</dt><dd>{item.generalizedRule}</dd></div>
                <div><dt>Root cause</dt><dd>{item.rootCause}</dd></div>
                <div><dt>Scope</dt><dd>{item.scope} · {item.kind}</dd></div>
                <div><dt>Confidence</dt><dd>{(item.confidenceMilli / 10).toFixed(1)}%</dd></div>
                <div><dt>Solution</dt><dd>{item.proposedSolution}</dd></div>
                <div><dt>Replay</dt><dd>{item.replay ? `${item.replay.reproduced ? "reproduced" : "not reproduced"}, ${item.replay.resolved ? "resolved" : "unresolved"}, ${item.replay.regressionCount} regressions` : "not run"}</dd></div>
              </dl>
              {!!item.conflictsWith.length && <p className="learning-conflict">Conflicts: {item.conflictsWith.join(", ")}</p>}
              <div className="learning-actions">
                {actions(item).map((action) => <button key={action} onClick={() => void act(item, action)} disabled={busy}>{action}</button>)}
                {item.state !== "deleted" && <button onClick={() => void act(item, "delete")} disabled={busy}>delete</button>}
              </div>
            </article>)}
            {!visible.length && <p className="learning-empty">No learning items match this view.</p>}
          </div>
        </>}
      </div>
    </div>}
  </>;
}
