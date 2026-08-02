import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  ArrowDownRight,
  ArrowUpRight,
  BarChart3,
  Beaker,
  CheckCircle2,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  X,
  XCircle,
} from "lucide-react";
import {
  generateImprovement,
  loadEngineeringDashboard,
  updateImprovement,
  type EngineeringDashboardData,
} from "./engineeringApi";
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
    catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };
  useEffect(() => { void reload(); }, [repo, days]);
  const act = async (id: string, action: "approve"|"reject"|"adopt"|"rollback"|"benchmark"|"suspend"|"supersede") => {
    setBusy(true);
    try { await updateImprovement(repo, id, action); await reload(); }
    catch (cause) { setError(String(cause)); setBusy(false); }
  };
  if (!repo) return <div className="engineering-empty">Open a repository to view its engineering dashboard.</div>;
  return <div className="engineering-dashboard">
    <header className="engineering-header"><div><span className="eyebrow">Medusa engineering system</span><h2>Factory Dashboard</h2><p>Measured outcomes, recurring friction, and guarded self-improvement.</p></div><div className="dashboard-actions"><select value={days} onChange={(event)=>setDays(Number(event.target.value))}><option value={30}>30 days</option><option value={90}>90 days</option><option value={365}>1 year</option></select><button onClick={()=>void reload()} disabled={busy}><RefreshCw size={15}/>Refresh</button><button className="primary-action" disabled={busy} onClick={async()=>{setBusy(true);try{await generateImprovement(repo);await reload();}catch(cause){setError(String(cause));setBusy(false);}}}><Sparkles size={15}/>Generate proposal</button></div></header>
    {error && <div className="engineering-error">{error}</div>}
    {data && <><div className="engineering-kpis"><div><span>Task success</span><strong>{data.totalTasks > 0 ? pct(data.successRate) : "No data"}</strong><small>{data.totalTasks > 0 ? `${data.successfulTasks}/${data.totalTasks} successful` : "No completed task data"}</small></div><div><span>Verification pass</span><strong>{pct(data.verificationPassRate)}</strong><small>Recorded verification events</small></div><div><span>Average retries</span><strong>{data.averageRetries.toFixed(2)}</strong><small>Per task</small></div><div><span>Human intervention</span><strong>{pct(data.humanInterventionRate)}</strong><small>Tasks paused for input</small></div><div><span>Average duration</span><strong>{data.averageDurationMinutes.toFixed(1)}m</strong><small>Session elapsed time</small></div><div><span>Rollback rate</span><strong>{pct(data.rollbackRate)}</strong><small>Improvement proposals</small></div></div><TrendChart data={data.trend}/><div className="engineering-columns"><section className="engineering-card"><div className="card-head"><div><span className="eyebrow">Bottlenecks</span><h3>Recurring friction</h3></div><Activity size={18}/></div><div className="friction-list">{data.friction.length ? data.friction.map((item)=><div key={item.category}><span>{item.category}</span><strong>{item.count}</strong><small>{item.sessions.length} affected session(s)</small></div>) : <p className="empty-state">No recurring friction recorded.</p>}</div></section><section className="engineering-card"><div className="card-head"><div><span className="eyebrow">Guarded evolution</span><h3>Improvement lifecycle</h3></div><ShieldCheck size={18}/></div><div className="proposal-list">{data.improvements.length ? data.improvements.slice().reverse().map((item)=><article key={item.id}><div className="proposal-title"><strong>{item.title}</strong><span className={`status ${item.status}`}>{item.status}</span></div><p>{item.problem}</p><small>{item.proposedChange}</small>{item.benchmarkBefore != null && <div className="benchmark-row"><span>Baseline {pct(item.benchmarkBefore)}</span>{item.benchmarkAfter != null && <span>After {pct(item.benchmarkAfter)}</span>}</div>}<div className="proposal-actions">{item.status === "pending" && <><button onClick={()=>void act(item.id,"approve")}><CheckCircle2 size={14}/>Approve</button><button onClick={()=>void act(item.id,"reject")}><XCircle size={14}/>Reject</button></>}{["approved","validated"].includes(item.status) && <><button onClick={()=>void act(item.id,"benchmark")}><Beaker size={14}/>Benchmark</button><button onClick={()=>void act(item.id,"adopt")}><Sparkles size={14}/>Adopt</button></>}{item.status === "active" && <><button onClick={()=>void act(item.id,"suspend")}><XCircle size={14}/>Suspend</button><button onClick={()=>void act(item.id,"rollback")}><RotateCcw size={14}/>Roll back</button></>}{item.status === "suspended" && <button onClick={()=>void act(item.id,"rollback")}><RotateCcw size={14}/>Roll back</button>}</div></article>) : <p className="empty-state">No proposals yet.</p>}</div></section></div></>}
  </div>;
}

export function EngineeringDashboardLauncher() {
  const [open, setOpen] = useState(false);
  const repo = window.localStorage.getItem("medusa.desktop.repo") ?? "";
  return <><button className="engineering-menu-button" onClick={()=>setOpen(true)} title="Engineering dashboard"><BarChart3 size={18}/><span>Engineering</span></button>{open && <div className="engineering-overlay"><div className="engineering-shell"><button className="engineering-close" onClick={()=>setOpen(false)} aria-label="Close engineering dashboard"><X size={18}/></button><Dashboard repo={repo}/></div></div>}</>;
}
