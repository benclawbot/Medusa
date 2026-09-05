import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  ArrowDownRight,
  ArrowUpRight,
  RefreshCw,
  ShieldCheck,
  X,
} from "lucide-react";
import {
  loadEngineeringDashboard,
  type EngineeringDashboardData,
} from "./engineeringApi";
import { useDockShell } from "./useDockShell";
import { toUserError } from "./errorPresentation";
import { REPO_CHANGED_EVENT } from "./runtime";
import "./engineering-dashboard.css";

const pct = (value: number) => `${value.toFixed(1)}%`;

function TrendChart({ data }: { data: EngineeringDashboardData["trend"] }) {
  const points = useMemo(() => {
    if (!data.length) return "";
    return data
      .map((point, index) => {
        const x = 22 + (index * 716) / Math.max(1, data.length - 1);
        const y = 198 - (point.successRate * 176) / 100;
        return `${x},${y}`;
      })
      .join(" ");
  }, [data]);
  const lastPoint = data[data.length - 1];
  const delta = (lastPoint?.successRate ?? 0) - (data[0]?.successRate ?? 0);
  return (
    <section className="engineering-card trend-card">
      <div className="card-head">
        <div><span className="eyebrow">Reliability evolution</span><h3>Task success rate over time</h3></div>
        <span className={`trend-delta ${delta >= 0 ? "positive" : "negative"}`}>
          {delta >= 0 ? <ArrowUpRight size={15} /> : <ArrowDownRight size={15} />} {Math.abs(delta).toFixed(1)} pts
        </span>
      </div>
      {data.length ? <><svg viewBox="0 0 760 220" role="img" aria-label="Task success rate trend"><g className="grid"><line x1="22" y1="22" x2="738" y2="22"/><line x1="22" y1="110" x2="738" y2="110"/><line x1="22" y1="198" x2="738" y2="198"/></g><polyline points={points}/>{data.map((point,index)=><circle key={point.date} cx={22+(index*716)/Math.max(1,data.length-1)} cy={198-(point.successRate*176)/100} r="4"><title>{point.date}: {pct(point.successRate)}</title></circle>)}</svg><div className="axis-labels"><span>{data[0]?.date}</span><span>{lastPoint?.date}</span></div></> : <p className="empty-state">No completed task data in this period.</p>}
    </section>
  );
}

function Dashboard({ repo }: { repo: string }) {
  const [days, setDays] = useState(90);
  const [data, setData] = useState<EngineeringDashboardData>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const reload = async () => {
    if (!repo) return;
    setBusy(true);
    try { setData(await loadEngineeringDashboard(repo, days)); setError(undefined); }
    catch (cause) { setError(toUserError(cause)); }
    finally { setBusy(false); }
  };
  useEffect(() => { void reload(); }, [repo, days]);
  if (!repo) return <div className="engineering-empty">Open a repository to view its engineering dashboard.</div>;
  return <div className="engineering-dashboard">
    <header className="engineering-header"><div><span className="eyebrow">Medusa engineering system</span><h2>Factory Dashboard</h2><p>Typed outcomes, recurring friction, and guarded self-improvement.</p><small>Canonical runtime candidates are read-only here. Use the shared learning commands for lifecycle changes.</small></div><div className="dashboard-actions"><select value={days} onChange={(event)=>setDays(Number(event.target.value))}><option value={30}>30 days</option><option value={90}>90 days</option><option value={365}>1 year</option></select><button onClick={()=>void reload()} disabled={busy}><RefreshCw size={15}/>Refresh</button></div></header>
    {error && <div className="engineering-error">{error}</div>}
    {data && <><div className="engineering-kpis"><div><span>Task success</span><strong>{data.totalTasks > 0 ? pct(data.successRate) : "No data"}</strong><small>{data.totalTasks > 0 ? `${data.successfulTasks}/${data.totalTasks} successful` : "No completed task data"}</small></div><div><span>Verification pass</span><strong>{pct(data.verificationPassRate)}</strong><small>Recorded verification events</small></div><div><span>Average retries</span><strong>{data.averageRetries.toFixed(2)}</strong><small>Per task</small></div><div><span>Human intervention</span><strong>{pct(data.humanInterventionRate)}</strong><small>Tasks paused for input</small></div><div><span>Average duration</span><strong>{data.averageDurationMinutes.toFixed(1)}m</strong><small>Typed duration is unavailable</small></div><div><span>Rollback rate</span><strong>{pct(data.rollbackRate)}</strong><small>Canonical runtime candidates</small></div></div><TrendChart data={data.trend}/><div className="engineering-columns"><section className="engineering-card"><div className="card-head"><div><span className="eyebrow">Bottlenecks</span><h3>Recurring friction</h3></div><Activity size={18}/></div><div className="friction-list">{data.friction.length ? data.friction.map((item)=><div key={item.category}><span>{item.category}</span><strong>{item.count}</strong><small>{item.sessions.length} affected session(s)</small></div>) : <p className="empty-state">No recurring friction recorded.</p>}</div></section><section className="engineering-card"><div className="card-head"><div><span className="eyebrow">Guarded evolution</span><h3>Canonical candidates</h3></div><ShieldCheck size={18}/></div><div className="proposal-list">{(data.improvements ?? []).length ? (data.improvements ?? []).slice().reverse().map((item)=><article key={item.id}><div className="proposal-title"><strong>{item.title}</strong><span className={`status ${item.status}`}>{item.status}</span></div><p>{item.problem}</p><small>{item.proposedChange}</small>{item.benchmarkBefore != null && <div className="benchmark-row"><span>Baseline {pct(item.benchmarkBefore)}</span>{item.benchmarkAfter != null && <span>After {pct(item.benchmarkAfter)}</span>}</div>}<small>Read-only compatibility record. Lifecycle changes must use /learning commands.</small></article>) : <p className="empty-state">No canonical candidates.</p>}</div></section></div></>}
  </div>;
}

export function EngineeringDashboardLauncher() {
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");
  const { open, setOpen, close, dialogRef } = useDockShell<HTMLDivElement>("engineering");
  useEffect(() => {
    const sync = () => setRepo(window.localStorage.getItem("medusa.desktop.repo") ?? "");
    sync();
    window.addEventListener(REPO_CHANGED_EVENT, sync);
    window.addEventListener("focus", sync);
    return () => {
      window.removeEventListener(REPO_CHANGED_EVENT, sync);
      window.removeEventListener("focus", sync);
    };
  }, []);
  return <>{open && <div className="engineering-overlay"><div ref={dialogRef} className="engineering-shell" role="dialog" aria-modal="true" aria-label="Engineering dashboard" tabIndex={-1}><button className="engineering-close" onClick={close} aria-label="Close engineering dashboard"><X size={18}/></button><Dashboard repo={repo}/></div></div>}</>;
}
