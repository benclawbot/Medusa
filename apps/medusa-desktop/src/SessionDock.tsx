import { LoaderCircle } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  REPO_CHANGED_EVENT,
  requestRuntimeResume,
  RUNTIME_DATA_CHANGED_EVENT,
  type SessionSummary,
} from "./runtime";
import { listRuntimeSessionPage } from "./sessionPaging";
import { toUserError } from "./errorPresentation";
import "./session-dock.css";

function currentRepo(): string {
  return window.localStorage.getItem("medusa.desktop.repo") ?? "";
}

export function formatSessionAge(value: string, now = Date.now()): string {
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "—";
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000));
  if (seconds < 60) return "now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** A compact recent-session list for the primary Medusa rail. */
export function SessionDock() {
  const [repo, setRepo] = useState(currentRepo);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(false);
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

  const refresh = useCallback(async () => {
    const generation = ++requestGeneration.current;
    if (!repo) {
      setSessions([]);
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
    } catch (cause) {
      if (generation === requestGeneration.current) setError(toUserError(cause));
    } finally {
      if (generation === requestGeneration.current) setLoading(false);
    }
  }, [repo]);

  useEffect(() => {
    void refresh();
    const refreshAfterRuntimeChange = () => void refresh();
    window.addEventListener(RUNTIME_DATA_CHANGED_EVENT, refreshAfterRuntimeChange);
    window.addEventListener("focus", refreshAfterRuntimeChange);
    return () => {
      window.removeEventListener(RUNTIME_DATA_CHANGED_EVENT, refreshAfterRuntimeChange);
      window.removeEventListener("focus", refreshAfterRuntimeChange);
    };
  }, [refresh]);

  return (
    <section className="recent-sessions" aria-label="Recent sessions">
      <div className="recent-sessions-heading">Recent</div>
      {loading && sessions.length === 0 && (
        <div className="recent-sessions-state" role="status"><LoaderCircle className="spin" size={13} /> Loading…</div>
      )}
      {!!error && <div className="recent-sessions-state error" role="alert">Unable to load recent sessions</div>}
      {!loading && !error && repo && sessions.length === 0 && (
        <div className="recent-sessions-state">No recent sessions</div>
      )}
      {sessions.slice(0, 8).map((session) => (
        <button
          className="recent-session-row"
          key={session.id}
          type="button"
          onClick={() => requestRuntimeResume(session.id, repo)}
          title={session.objective || "Untitled session"}
        >
          <span>{session.objective || "Untitled session"}</span>
          <time dateTime={session.updatedAt}>{formatSessionAge(session.updatedAt)}</time>
        </button>
      ))}
    </section>
  );
}
