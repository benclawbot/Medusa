import {
  ArrowLeft,
  CheckCircle2,
  Clock3,
  History,
  LoaderCircle,
  MessageCircleQuestion,
  Play,
  RefreshCw,
  X,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  listRuntimeSessions,
  readRuntimeSession,
  requestRuntimeResume,
  type SessionDetail,
  type SessionSummary,
} from "./runtime";
import "./session-dock.css";

function currentRepo(): string {
  return window.localStorage.getItem("medusa.desktop.repo") ?? "";
}

export function formatSessionAge(value: string, now = Date.now()): string {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "unknown";
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function sessionStatus(session: SessionSummary): { label: string; className: string } {
  if (session.waitingForUser) return { label: "Needs input", className: "waiting" };
  if (session.completed) return { label: "Completed", className: "completed" };
  return { label: "In progress", className: "active" };
}

export function SessionDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(currentRepo);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selected, setSelected] = useState<SessionDetail>();
  const [loading, setLoading] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    const interval = window.setInterval(() => {
      const next = currentRepo();
      setRepo((current) => current === next ? current : next);
    }, 750);
    return () => window.clearInterval(interval);
  }, []);

  useEffect(() => {
    setSelected(undefined);
  }, [repo]);

  const refresh = useCallback(async () => {
    if (!repo) {
      setSessions([]);
      setError(undefined);
      return;
    }
    setLoading(true);
    setError(undefined);
    try {
      setSessions(await listRuntimeSessions(repo));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, [repo]);

  const openSession = useCallback(async (sessionId: string) => {
    setDetailLoading(true);
    setError(undefined);
    try {
      setSelected(await readRuntimeSession(repo, sessionId));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setDetailLoading(false);
    }
  }, [repo]);

  const resumeSession = useCallback(() => {
    if (!selected) return;
    requestRuntimeResume(selected.summary.id);
    window.location.reload();
  }, [selected]);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  if (!repo) return null;

  return (
    <div className={`session-dock${open ? " open" : ""}`}>
      <button
        className="session-dock-trigger"
        type="button"
        aria-label="Open recent sessions"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <History size={16} />
        <span>Sessions</span>
      </button>

      {open && (
        <section className="session-dock-panel" aria-label="Recent Medusa sessions">
          <header>
            <div>
              <small>{selected ? "Saved conversation" : "Current project"}</small>
              <strong>{selected ? selected.summary.objective || "Untitled session" : "Recent sessions"}</strong>
            </div>
            <div className="session-dock-actions">
              {selected && (
                <button type="button" onClick={() => setSelected(undefined)} aria-label="Back to sessions">
                  <ArrowLeft size={14} />
                </button>
              )}
              {!selected && (
                <button type="button" onClick={() => void refresh()} disabled={loading} aria-label="Refresh sessions">
                  <RefreshCw size={14} className={loading ? "spin" : undefined} />
                </button>
              )}
              <button type="button" onClick={() => setOpen(false)} aria-label="Close recent sessions">
                <X size={15} />
              </button>
            </div>
          </header>

          {selected ? (
            <div className="session-history">
              <div className="session-history-meta">
                <span>Turn {selected.summary.turn}</span>
                <span>{formatSessionAge(selected.summary.updatedAt)}</span>
                <code>{selected.summary.id.slice(0, 8)}</code>
              </div>
              {selected.messages.length ? selected.messages.map((message, index) => (
                <article className={`session-history-message ${message.role}`} key={`${message.role}-${index}`}>
                  <small>{message.role === "assistant" ? "Medusa" : message.role === "user" ? "You" : message.role}</small>
                  <p>{message.text}</p>
                </article>
              )) : (
                <div className="session-dock-empty"><History size={18} /> No durable messages in this session.</div>
              )}
            </div>
          ) : (
            <div className="session-dock-list">
              {(loading || detailLoading) && sessions.length === 0 && (
                <div className="session-dock-empty"><LoaderCircle className="spin" size={18} /> Loading sessions…</div>
              )}
              {!!error && <div className="session-dock-error">{error}</div>}
              {!loading && !error && sessions.length === 0 && (
                <div className="session-dock-empty"><History size={18} /> No saved sessions for this project.</div>
              )}
              {sessions.slice(0, 12).map((session) => {
                const status = sessionStatus(session);
                return (
                  <button
                    className="session-dock-item"
                    key={session.id}
                    type="button"
                    onClick={() => void openSession(session.id)}
                    disabled={detailLoading}
                  >
                    <div className="session-dock-item-top">
                      <strong>{session.objective || "Untitled session"}</strong>
                      <span className={`session-status ${status.className}`}>
                        {session.waitingForUser ? <MessageCircleQuestion size={12} /> : session.completed ? <CheckCircle2 size={12} /> : <Clock3 size={12} />}
                        {status.label}
                      </span>
                    </div>
                    <div className="session-dock-meta">
                      <span>Turn {session.turn}</span>
                      <span>{formatSessionAge(session.updatedAt)}</span>
                      <code>{session.id.slice(0, 8)}</code>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
          <footer>
            {selected ? (
              <button type="button" className="session-resume" onClick={resumeSession}>
                <Play size={14} /> Resume session
              </button>
            ) : (
              <span>Select a session to inspect its durable conversation.</span>
            )}
          </footer>
        </section>
      )}
    </div>
  );
}
