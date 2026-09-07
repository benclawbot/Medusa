import {
  ArrowLeft,
  CheckCircle2,
  ChevronDown,
  Clock3,
  History,
  LoaderCircle,
  MessageCircleQuestion,
  Play,
  RefreshCw,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  REPO_CHANGED_EVENT,
  requestRuntimeResume,
  RUNTIME_DATA_CHANGED_EVENT,
  type SessionDetail,
  type SessionSummary,
} from "./runtime";
import {
  listRuntimeSessionPage,
  readRuntimeSessionPage,
} from "./sessionPaging";
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

/**
 * The session browser is intentionally rendered in the rail. Keeping it mounted there makes
 * the current-session list immediately visible and avoids a competing modal surface.
 */
export function SessionDock() {
  const [repo, setRepo] = useState(currentRepo);
  const [expanded, setExpanded] = useState(true);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [sessionCursor, setSessionCursor] = useState<string>();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<SessionDetail>();
  const [messageCursor, setMessageCursor] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string>();
  const requestGeneration = useRef(0);

  useEffect(() => {
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
  }, []);

  useEffect(() => {
    setSelected(undefined);
    setMessageCursor(undefined);
    setSessions([]);
    setSessionCursor(undefined);
    setQuery("");
    setError(undefined);
  }, [repo]);

  const refresh = useCallback(async () => {
    const generation = ++requestGeneration.current;
    if (!repo) {
      setSessions([]);
      setSessionCursor(undefined);
      setLoading(false);
      setError(undefined);
      return;
    }
    setLoading(true);
    setError(undefined);
    try {
      const page = await listRuntimeSessionPage(repo);
      if (generation !== requestGeneration.current) return;
      setSessions(page.sessions);
      setSessionCursor(page.nextCursor);
    } catch (cause) {
      if (generation === requestGeneration.current) setError(toUserError(cause));
    } finally {
      if (generation === requestGeneration.current) setLoading(false);
    }
  }, [repo]);

  useEffect(() => {
    if (!expanded) return;
    void refresh();
    const refreshAfterRuntimeChange = () => void refresh();
    window.addEventListener(RUNTIME_DATA_CHANGED_EVENT, refreshAfterRuntimeChange);
    window.addEventListener("focus", refreshAfterRuntimeChange);
    return () => {
      window.removeEventListener(RUNTIME_DATA_CHANGED_EVENT, refreshAfterRuntimeChange);
      window.removeEventListener("focus", refreshAfterRuntimeChange);
    };
  }, [expanded, refresh]);

  const loadMoreSessions = useCallback(async () => {
    if (!repo || !sessionCursor || loading) return;
    const generation = ++requestGeneration.current;
    setLoading(true);
    setError(undefined);
    try {
      const page = await listRuntimeSessionPage(repo, sessionCursor);
      if (generation !== requestGeneration.current) return;
      setSessions((current) => {
        const seen = new Set(current.map((item) => item.id));
        return [...current, ...page.sessions.filter((item) => !seen.has(item.id))];
      });
      setSessionCursor(page.nextCursor);
    } catch (cause) {
      if (generation === requestGeneration.current) setError(toUserError(cause));
    } finally {
      if (generation === requestGeneration.current) setLoading(false);
    }
  }, [loading, repo, sessionCursor]);

  const openSession = useCallback(async (sessionId: string) => {
    const generation = ++requestGeneration.current;
    setDetailLoading(true);
    setError(undefined);
    try {
      const page = await readRuntimeSessionPage(repo, sessionId);
      if (generation !== requestGeneration.current) return;
      setSelected({ summary: page.summary, messages: page.messages });
      setMessageCursor(page.nextCursor);
    } catch (cause) {
      if (generation === requestGeneration.current) setError(toUserError(cause));
    } finally {
      if (generation === requestGeneration.current) setDetailLoading(false);
    }
  }, [repo]);

  const loadOlderMessages = useCallback(async () => {
    if (!selected || !messageCursor || detailLoading) return;
    const generation = ++requestGeneration.current;
    setDetailLoading(true);
    setError(undefined);
    try {
      const page = await readRuntimeSessionPage(repo, selected.summary.id, messageCursor);
      if (generation !== requestGeneration.current) return;
      setSelected((current) => current && current.summary.id === page.summary.id
        ? { ...current, messages: [...page.messages, ...current.messages] }
        : current);
      setMessageCursor(page.nextCursor);
    } catch (cause) {
      if (generation === requestGeneration.current) setError(toUserError(cause));
    } finally {
      if (generation === requestGeneration.current) setDetailLoading(false);
    }
  }, [detailLoading, messageCursor, repo, selected]);

  const resumeSession = useCallback(() => {
    if (!selected) return;
    requestRuntimeResume(selected.summary.id, repo);
  }, [repo, selected]);

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const visibleSessions = normalizedQuery
    ? sessions.filter((session) => session.objective.toLocaleLowerCase().includes(normalizedQuery))
    : sessions;

  return (
    <section className={`sessions-inline ${expanded ? "expanded" : "collapsed"}`} aria-label="Sessions">
      <button
        className="nav-item sessions-inline-toggle"
        type="button"
        onClick={() => setExpanded((current) => !current)}
        title="Sessions"
        aria-expanded={expanded}
        aria-controls="sessions-inline-content"
      >
        <History size={17} />
        <span className="rail-label">Sessions</span>
        <ChevronDown className={`session-chevron rail-label${expanded ? " expanded" : ""}`} size={15} aria-hidden="true" />
      </button>

      {expanded && (
        <div className="sessions-inline-content" id="sessions-inline-content">
          {selected ? (
            <>
              <div className="session-inline-heading">
                <button type="button" onClick={() => { setSelected(undefined); setMessageCursor(undefined); }} aria-label="Back to sessions">
                  <ArrowLeft size={13} />
                </button>
                <strong title={selected.summary.objective}>{selected.summary.objective || "Untitled session"}</strong>
              </div>
              <div className="session-history">
                <div className="session-history-meta">
                  <span>Turn {selected.summary.turn}</span>
                  <span>{formatSessionAge(selected.summary.updatedAt)}</span>
                  <code>{(selected.summary.id ?? "").slice(0, 8) || "unavailable"}</code>
                </div>
                {messageCursor && (
                  <button type="button" className="session-resume" onClick={() => void loadOlderMessages()} disabled={detailLoading}>
                    {detailLoading ? <LoaderCircle className="spin" size={13} /> : <History size={13} />} Load older messages
                  </button>
                )}
                {selected.messages.length ? selected.messages.map((message, index) => (
                  <article className={`session-history-message ${message.role}`} key={`${message.role}-${index}-${message.text.slice(0, 24)}`}>
                    <small>{message.role === "assistant" ? "Medusa" : message.role === "user" ? "You" : message.role}</small>
                    <p>{message.text}</p>
                  </article>
                )) : (
                  <div className="session-dock-empty"><History size={16} /> No durable messages.</div>
                )}
              </div>
              <button type="button" className="session-resume" onClick={resumeSession}>
                <Play size={13} /> Resume session
              </button>
            </>
          ) : (
            <>
              <div className="session-inline-heading">
                <span><strong>{repo ? "Recent sessions" : "General chat"}</strong><small>{repo ? "Saved for this project" : "Open a project to browse saved sessions"}</small></span>
                <button type="button" onClick={() => void refresh()} disabled={loading} aria-label="Refresh sessions" title="Refresh sessions">
                  <RefreshCw size={13} className={loading ? "spin" : undefined} />
                </button>
              </div>
              <label className="session-search">
                <span className="visually-hidden">Search saved sessions</span>
                <input
                  type="search"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Search sessions"
                  aria-label="Search saved sessions"
                />
              </label>
              {loading && sessions.length === 0 && (
                <div className="session-dock-empty"><LoaderCircle className="spin" size={16} /> Loading sessions…</div>
              )}
              {!!error && <div className="session-dock-error">{error}</div>}
              {!loading && !error && sessions.length === 0 && (
                <div className="session-dock-empty"><History size={16} /> No saved sessions for this project.</div>
              )}
              {!loading && !error && sessions.length > 0 && visibleSessions.length === 0 && (
                <div className="session-dock-empty"><History size={16} /> No sessions match “{query}”.</div>
              )}
              {visibleSessions.map((session) => {
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
                        {session.waitingForUser ? <MessageCircleQuestion size={11} /> : session.completed ? <CheckCircle2 size={11} /> : <Clock3 size={11} />}
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
                  {loading ? <LoaderCircle className="spin" size={13} /> : <History size={13} />} Load older sessions
                </button>
              )}
            </>
          )}
        </div>
      )}
    </section>
  );
}
