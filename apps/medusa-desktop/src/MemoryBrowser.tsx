import { Archive, BookOpen, CheckCircle2, Database, RefreshCw, Search, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { listRuntimeMemories, type DesktopMemory } from "./runtime";

interface MemoryBrowserProps {
  repo: string;
}

export function MemoryBrowser({ repo }: MemoryBrowserProps) {
  const [items, setItems] = useState<DesktopMemory[]>([]);
  const [selectedId, setSelectedId] = useState<string>();
  const [query, setQuery] = useState("");
  const [includeInactive, setIncludeInactive] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();

  const refresh = async () => {
    if (!repo) return;
    setLoading(true);
    setError(undefined);
    try {
      const memories = await listRuntimeMemories(repo, query, includeInactive);
      setItems(memories);
      setSelectedId((current) => memories.some((item) => item.id === current) ? current : memories[0]?.id);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void refresh(); }, [repo, includeInactive]);

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? items[0],
    [items, selectedId],
  );

  if (!repo) {
    return <div className="memory-empty"><Database size={28} /><h2>Open a project to browse memory</h2><p>Memory is repository-scoped and stored as canonical Markdown under <code>.medusa/memory</code>.</p></div>;
  }

  return (
    <div className="memory-browser">
      <header className="memory-toolbar">
        <div><h2><BookOpen size={18} /> Memory</h2><p>Canonical claims with validation provenance and source evidence</p></div>
        <button onClick={() => void refresh()} disabled={loading} aria-label="Refresh memory"><RefreshCw size={15} /> Refresh</button>
      </header>
      <div className="memory-controls">
        <label className="memory-search"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void refresh(); }} placeholder="Search claims, tags, or sources" /></label>
        <label className="memory-toggle"><input type="checkbox" checked={includeInactive} onChange={(event) => setIncludeInactive(event.target.checked)} /> Include archived and superseded</label>
      </div>
      {error && <div className="memory-error">{error}</div>}
      <div className="memory-layout">
        <aside className="memory-list" aria-label="Memory documents">
          {loading && !items.length && <p className="memory-muted">Loading memory…</p>}
          {!loading && !items.length && <p className="memory-muted">No matching canonical memory documents.</p>}
          {items.map((item) => (
            <button key={item.id} className={item.id === selected?.id ? "active" : ""} onClick={() => setSelectedId(item.id)}>
              <span className="memory-list-title">{item.title}</span>
              <span className="memory-list-meta"><em>{item.memoryType}</em><small>{item.validation}</small></span>
              <span className="memory-list-path">{item.path}</span>
            </button>
          ))}
        </aside>
        <section className="memory-detail" aria-live="polite">
          {selected ? <>
            <div className="memory-detail-heading">
              <div><span className={`memory-status ${selected.status}`}>{selected.status === "active" ? <CheckCircle2 size={13} /> : <Archive size={13} />}{selected.status}</span><h2>{selected.title}</h2><code>{selected.id}</code></div>
              <div className="memory-confidence"><strong>{(selected.confidenceMilli / 10).toFixed(0)}%</strong><small>confidence</small></div>
            </div>
            <div className="memory-provenance-grid">
              <div><small>Validation</small><strong><ShieldCheck size={14} /> {selected.validation}</strong></div>
              <div><small>Scope</small><strong>{selected.scope}</strong></div>
              <div><small>Last validated</small><strong>{selected.lastValidatedAt}</strong></div>
              <div><small>Successful reuse</small><strong>{selected.successfulReuseCount}</strong></div>
              <div><small>Session</small><strong>{selected.sessionId ?? "—"}</strong></div>
              <div><small>Updated</small><strong>{selected.updatedAt}</strong></div>
            </div>
            <article className="memory-body">{selected.body}</article>
            {!!selected.tags.length && <div className="memory-tags">{selected.tags.map((tag) => <span key={tag}>{tag}</span>)}</div>}
            <section className="memory-sources"><h3>Source evidence</h3>{selected.sources.length ? selected.sources.map((source) => <code key={source}>{source}</code>) : <p>No source evidence recorded.</p>}</section>
            {(selected.supersedes.length > 0 || selected.supersededBy.length > 0) && <section className="memory-lineage"><h3>Lineage</h3>{selected.supersedes.map((id) => <p key={id}>Supersedes <code>{id}</code></p>)}{selected.supersededBy.map((id) => <p key={id}>Superseded by <code>{id}</code></p>)}</section>}
          </> : <div className="memory-empty compact"><Database size={24} /><p>Select a memory document.</p></div>}
        </section>
      </div>
    </div>
  );
}
