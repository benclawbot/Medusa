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
  REPO_CHANGED_EVENT,
  requestRuntimeResume,
  type SessionDetail,
  type SessionSummary,
} from "./runtime";
import {
  listRuntimeSessionPage,
  readRuntimeSessionPage,
} from "./sessionPaging";
import { useDockShell } from "./useDockShell";
import { toUserError } from "./errorPresentation";
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
  const [repo, setRepo] = useState(currentRepo);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [sessionCursor, setSessionCursor] = useState<string>();
  const [selected, setSelected] = useState<SessionDetail>();
  const [messageCursor, setMessageCursor] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const { open, setOpen, error, setError, dialogRef } = useDockShell<HTMLElement>("sessions");

  useEffect(() => {
    if (!open) return;
    const sync = () => {
      const next = currentRepo();
      setRepo((current) => current === next ? current : next);
    };
    sync();
    window.addEventListener("focus", sync);
    window.addEventListener(REPO_CHANGED_EVENT, sync);
    return () => {
      window.removeEventListener("focus", sync);
      window.removeEventListener(REPO_CHANGED_EVENT, sync);
    };
  }, [open]);

  useEffect(() => {
    setSelected(undefined);
    setMessageCursor(undefined);
    setSessions([]);
    setSessionCursor(undefined);
  }, [repo]);

  const refresh = useCallback(async () => {
    if (!repo) {
      setSessions([]);
      setSessionCursor(undefined);
      setError(undefined);
      return;
    }
    setLoading(true);
    setError(undefined);
    try {
      const page = await listRuntimeSessionPage(repo);
      setSessions(page.sessions);
      setSessionCursor(page.nextCursor);
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setLoading(false);
    }
  }, [repo, setError]);

  const loadMoreSessions = useCallback(async () => {
    if (!repo || !sessionCursor || loading) return;
    setLoading(true);
    setError(undefined);
    try {
      const page = await listRuntimeSessionPage(repo, sessionCursor);
      setSessions((current) => {
        const seen = new Set(current.map((item) => item.id));
        return [...current, ...page.sessions.filter((item) => !seen.has(item.id))];
      });
      setSessionCursor(page.nextCursor);
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setLoading(false);
    }
  }, [repo, sessionCursor, loading, setError]);

  const openSession = useCallback(async (sessionId: string) => {
    setDetailLoading(true);
    setError(undefined);
    try {
      const page = await readRuntimeSessionPage(repo, sessionId);
      setSelected({ summary: page.summary, messages: page.messages });
      setMessageCursor(page.nextCursor);
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setDetailLoading(false);
    }
  }, [repo, setError]);

  const loadOlderMessages = useCallback(async () => {
    if (!selected || !messageCursor || detailLoading) return;
    setDetailLoading(true);
    setError(undefined);
    try {
      const page = await readRuntimeSessionPage(repo, selected.summary.id, messageCursor);
      setSelected((current) => current && current.summary.id === page.summary.id
        ? { ...current, messages: [...page.messages, ...current.messages] }
        : current);
      setMessageCursor(page.nextCursor);
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setDetailLoading(false);
    }
  }, [repo, selected, messageCursor, detailLoading, setError]);

  const resumeSession = useCallback(() => {
    if (!selected) return;
    requestRuntimeResume(selected.summary.id);
  }, [selected]);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  return (
    open ? (
      <div className="session-dock open">
        <section ref={dialogRef} className="session-dock-panel" role="dialog" aria-modal="true" aria-label="Recent Medusa sessions" tabIndex={-1}>
          <header>
            <div>
              <small>{selected ? "Saved conversation" : "Current project"}</small>
              <strong>{selected ? selected.summary.objective || "Untitled session" : "Recent sessions"}</strong>
            </div>
            <div className="session-dock-actions">
              {selected && (
                <button type="button" onClick={() => { setSelected(undefined); setMessageCursor(undefined); }} aria-label="Back to sessions">
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
                <code>{(selected.summary.id ?? "").slice(0, 8) || "unavailable"}</code>
              </div>
              {messageCursor && (
                <button type="button" className="session-resume" onClick={() => void loadOlderMessages()} disabled={detailLoading}>
                  {detailLoading ? <LoaderCircle className="spin" size={14} /> : <History size={14} />} Load older messages
                </button>
              )}
              {selected.messages.length ? selected.messages.map((message, index) => (
                <article className={`session-history-message ${message.role}`} key={`${message.role}-${index}-${message.text.slice(0, 24)}`}>
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
              {(sessions ?? []).map((session) => {
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
                      <code>{(session.id ?? "").slice(0, 8) || "unavailable"}</code>
                    </div>
                  </button>
                );
              })}
              {sessionCursor && (
                <button type="button" className="session-resume" onClick={() => void loadMoreSessions()} disabled={loading}>
                  {loading ? <LoaderCircle className="spin" size={14} /> : <History size={14} />} Load older sessions
                </button>
              )}
            </div>
          )}
          <footer>
            {selected ? (
              <button type="button" className="session-resume" onClick={resumeSession}>
                <Play size={14} /> Resume session
              </button>
            ) : (
              <span>Sessions are loaded in bounded pages; older history remains available.</span>
            )}
          </footer>
        </section>
      </div>
    ) : null
  );
}
