import {
  Archive,
  Check,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  Pin,
  Trash2,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  REPO_CHANGED_EVENT,
  requestRuntimeResume,
  RUNTIME_DATA_CHANGED_EVENT,
  type SessionSummary,
} from "./runtime";
import {
  archiveRuntimeSession,
  deleteRuntimeSessions,
  listRuntimeSessionPage,
  renameRuntimeSession,
  setRuntimeSessionPinned,
} from "./sessionPaging";
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

function sessionTitle(session: SessionSummary): string {
  return session.objective || "Untitled session";
}

/** A compact recent-session list for the primary Medusa rail. */
export function SessionDock() {
  const [repo, setRepo] = useState(currentRepo);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [actionMenuId, setActionMenuId] = useState<string>();
  const [headerMenuOpen, setHeaderMenuOpen] = useState(false);
  const [selectionMode, setSelectionMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const requestGeneration = useRef(0);

  const visibleSessions = sessions.slice(0, 8);

  useEffect(() => {
    const sync = () => {
      const next = currentRepo();
      setRepo((current) => current === next ? current : next);
      setActionMenuId(undefined);
      setHeaderMenuOpen(false);
      setSelectionMode(false);
      setSelected(new Set());
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
      const ids = new Set(page.sessions.slice(0, 8).map((session) => session.id));
      setSelected((current) => new Set([...current].filter((id) => ids.has(id))));
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

  const runSessionAction = async (action: () => Promise<void>) => {
    setError(undefined);
    try {
      await action();
      setActionMenuId(undefined);
      await refresh();
    } catch (cause) {
      setError(toUserError(cause));
    }
  };

  const rename = async (session: SessionSummary) => {
    const title = window.prompt("Rename session", sessionTitle(session));
    if (title === null) return;
    await runSessionAction(() => renameRuntimeSession(repo, session.id, title));
  };

  const startSelection = (selectAll: boolean) => {
    setSelectionMode(true);
    setHeaderMenuOpen(false);
    setActionMenuId(undefined);
    setSelected(selectAll ? new Set(visibleSessions.map((session) => session.id)) : new Set());
  };

  const deleteSelected = async () => {
    const ids = [...selected];
    if (!ids.length) return;
    setError(undefined);
    try {
      await deleteRuntimeSessions(repo, ids);
      setSelectionMode(false);
      setSelected(new Set());
      await refresh();
    } catch (cause) {
      setError(toUserError(cause));
    }
  };

  return (
    <section className="recent-sessions" aria-label="Recent sessions">
      <div className="recent-sessions-heading">
        <span>Recent</span>
        {!!repo && visibleSessions.length > 0 && (
          <div className="recent-heading-actions">
            <button
              className="recent-heading-menu-trigger"
              type="button"
              aria-label="Recent session options"
              aria-haspopup="menu"
              aria-expanded={headerMenuOpen}
              onClick={() => {
                setHeaderMenuOpen((current) => !current);
                setActionMenuId(undefined);
              }}
            >
              <MoreHorizontal size={16} />
            </button>
            {headerMenuOpen && (
              <div className="recent-session-menu recent-heading-menu" role="menu" aria-label="Recent selection options">
                <button type="button" role="menuitem" onClick={() => startSelection(true)}>
                  <Check size={14} /> Select all
                </button>
                <button type="button" role="menuitem" onClick={() => startSelection(false)}>
                  <span className="recent-menu-icon-placeholder" /> Select none
                </button>
              </div>
            )}
          </div>
        )}
      </div>

      {loading && sessions.length === 0 && (
        <div className="recent-sessions-state" role="status"><LoaderCircle className="spin" size={13} /> Loading…</div>
      )}
      {!!error && <div className="recent-sessions-state error" role="alert">Unable to update recent sessions</div>}
      {!loading && !error && repo && sessions.length === 0 && (
        <div className="recent-sessions-state">No recent sessions</div>
      )}

      {visibleSessions.map((session) => {
        const title = sessionTitle(session);
        if (selectionMode) {
          return (
            <label className="recent-session-row recent-session-select-row" key={session.id}>
              <input
                type="checkbox"
                aria-label={`Select ${title}`}
                checked={selected.has(session.id)}
                onChange={(event) => {
                  setSelected((current) => {
                    const next = new Set(current);
                    if (event.target.checked) next.add(session.id);
                    else next.delete(session.id);
                    return next;
                  });
                }}
              />
              <span className="recent-session-title">{title}</span>
              <time dateTime={session.updatedAt}>{formatSessionAge(session.updatedAt)}</time>
            </label>
          );
        }

        return (
          <div className="recent-session-row" key={session.id}>
            <button
              className="recent-session-open"
              type="button"
              onClick={() => requestRuntimeResume(session.id, repo)}
              title={title}
            >
              {session.pinned && <Pin className="recent-session-pin" size={12} aria-label="Pinned" />}
              <span className="recent-session-title">{title}</span>
              <time dateTime={session.updatedAt}>{formatSessionAge(session.updatedAt)}</time>
            </button>
            <div className="recent-session-actions">
              <button
                className="recent-session-menu-trigger"
                type="button"
                aria-label={`Actions for ${title}`}
                aria-haspopup="menu"
                aria-expanded={actionMenuId === session.id}
                onClick={() => {
                  setActionMenuId((current) => current === session.id ? undefined : session.id);
                  setHeaderMenuOpen(false);
                }}
              >
                <MoreHorizontal size={16} />
              </button>
              {actionMenuId === session.id && (
                <div className="recent-session-menu" role="menu" aria-label={`Actions for ${title}`}>
                  <button type="button" role="menuitem" onClick={() => void rename(session)}>
                    <Pencil size={14} /> Rename
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => void runSessionAction(() => setRuntimeSessionPinned(repo, session.id, !session.pinned))}
                  >
                    <Pin size={14} /> {session.pinned ? "Unpin chat" : "Pin chat"}
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => void runSessionAction(() => archiveRuntimeSession(repo, session.id))}
                  >
                    <Archive size={14} /> Archive
                  </button>
                  <button
                    className="danger"
                    type="button"
                    role="menuitem"
                    onClick={() => void runSessionAction(() => deleteRuntimeSessions(repo, [session.id]))}
                  >
                    <Trash2 size={14} /> Delete
                  </button>
                </div>
              )}
            </div>
          </div>
        );
      })}

      {selectionMode && (
        <div className="recent-selection-toolbar" role="group" aria-label="Session selection actions">
          <button
            className="recent-delete-selected"
            type="button"
            disabled={selected.size === 0}
            onClick={() => void deleteSelected()}
          >
            <Trash2 size={13} /> Delete selected{selected.size ? ` (${selected.size})` : ""}
          </button>
          <button
            type="button"
            onClick={() => {
              setSelectionMode(false);
              setSelected(new Set());
            }}
          >
            Cancel
          </button>
        </div>
      )}
    </section>
  );
}
