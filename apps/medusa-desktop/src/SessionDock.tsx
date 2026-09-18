import { Archive, Ellipsis, LoaderCircle, Pencil, Pin, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  REPO_CHANGED_EVENT,
  requestRuntimeResume,
  RUNTIME_DATA_CHANGED_EVENT,
  type SessionSummary,
} from "./runtime";
import { listRuntimeSessionPage } from "./sessionPaging";
import { toUserError } from "./errorPresentation";
import "./session-dock.css";

interface SessionUiEntry {
  title?: string;
  pinned?: boolean;
  archived?: boolean;
  deleted?: boolean;
}

type SessionUiState = Record<string, SessionUiEntry>;

const SESSION_UI_VERSION = 1;

interface PersistedSessionUiState {
  version: typeof SESSION_UI_VERSION;
  entries: SessionUiState;
}

function currentRepo(): string {
  return window.localStorage.getItem("medusa.desktop.repo") ?? "";
}

function sessionUiKey(repo: string): string {
  return `medusa.desktop.session-ui.v${SESSION_UI_VERSION}:${encodeURIComponent(repo)}`;
}

function loadSessionUi(repo: string): SessionUiState {
  if (!repo) return {};
  try {
    const raw = window.localStorage.getItem(sessionUiKey(repo));
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Partial<PersistedSessionUiState>;
    if (parsed.version !== SESSION_UI_VERSION || !parsed.entries || typeof parsed.entries !== "object") return {};
    const entries: SessionUiState = {};
    for (const [id, value] of Object.entries(parsed.entries)) {
      if (!value || typeof value !== "object") continue;
      const entry = value as SessionUiEntry;
      entries[id] = {
        title: typeof entry.title === "string" ? entry.title : undefined,
        pinned: entry.pinned === true,
        archived: entry.archived === true,
        deleted: entry.deleted === true,
      };
    }
    return entries;
  } catch {
    return {};
  }
}

function persistSessionUi(repo: string, entries: SessionUiState): void {
  if (!repo) return;
  const payload: PersistedSessionUiState = { version: SESSION_UI_VERSION, entries };
  window.localStorage.setItem(sessionUiKey(repo), JSON.stringify(payload));
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
  const [sessionUi, setSessionUi] = useState<SessionUiState>(() => loadSessionUi(currentRepo()));
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [openMenuId, setOpenMenuId] = useState<string>();
  const [headingMenuOpen, setHeadingMenuOpen] = useState(false);
  const [selectionMode, setSelectionMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [renamingSession, setRenamingSession] = useState<string>();
  const [renameText, setRenameText] = useState("");
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
    setSessionUi(loadSessionUi(repo));
    setOpenMenuId(undefined);
    setHeadingMenuOpen(false);
    setSelectionMode(false);
    setSelected(new Set());
    setRenamingSession(undefined);
  }, [repo]);

  const commitSessionUi = useCallback((mutate: (current: SessionUiState) => SessionUiState) => {
    setSessionUi((current) => {
      const next = mutate(current);
      persistSessionUi(repo, next);
      return next;
    });
  }, [repo]);

  const updateSessionUi = useCallback((sessionId: string, patch: Partial<SessionUiEntry>) => {
    commitSessionUi((current) => ({
      ...current,
      [sessionId]: {
        ...current[sessionId],
        ...patch,
      },
    }));
  }, [commitSessionUi]);

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

  const visibleSessions = useMemo(() => sessions
    .map((session, index) => ({ session, index }))
    .filter(({ session }) => {
      const ui = sessionUi[session.id];
      return !ui?.archived && !ui?.deleted;
    })
    .sort((left, right) => {
      const leftPinned = sessionUi[left.session.id]?.pinned === true;
      const rightPinned = sessionUi[right.session.id]?.pinned === true;
      if (leftPinned !== rightPinned) return leftPinned ? -1 : 1;
      return left.index - right.index;
    })
    .map(({ session }) => session), [sessions, sessionUi]);

  const displayedSessions = selectionMode ? visibleSessions : visibleSessions.slice(0, 8);

  const titleFor = useCallback((session: SessionSummary) => (
    sessionUi[session.id]?.title?.trim() || session.objective || "Untitled session"
  ), [sessionUi]);

  const toggleSelection = useCallback((sessionId: string) => {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(sessionId)) next.delete(sessionId);
      else next.add(sessionId);
      return next;
    });
  }, []);

  const selectAll = () => {
    setSelectionMode(true);
    setSelected(new Set(visibleSessions.map((session) => session.id)));
    setHeadingMenuOpen(false);
    setOpenMenuId(undefined);
  };

  const selectNone = () => {
    setSelectionMode(true);
    setSelected(new Set());
    setHeadingMenuOpen(false);
    setOpenMenuId(undefined);
  };

  const deleteSelected = () => {
    if (!selected.size) return;
    const selectedIds = new Set(selected);
    commitSessionUi((current) => {
      const next = { ...current };
      for (const id of selectedIds) {
        next[id] = { ...next[id], deleted: true };
      }
      return next;
    });
    setSelected(new Set());
    setSelectionMode(false);
    setHeadingMenuOpen(false);
  };

  const beginRename = (session: SessionSummary) => {
    setRenameText(titleFor(session));
    setRenamingSession(session.id);
    setOpenMenuId(undefined);
  };

  const finishRename = (sessionId: string) => {
    const title = renameText.trim();
    updateSessionUi(sessionId, { title: title || undefined });
    setRenamingSession(undefined);
    setRenameText("");
  };

  const archiveSession = (sessionId: string) => {
    updateSessionUi(sessionId, { archived: true });
    setOpenMenuId(undefined);
    setSelected((current) => {
      const next = new Set(current);
      next.delete(sessionId);
      return next;
    });
  };

  const deleteSession = (sessionId: string) => {
    updateSessionUi(sessionId, { deleted: true });
    setOpenMenuId(undefined);
    setSelected((current) => {
      const next = new Set(current);
      next.delete(sessionId);
      return next;
    });
  };

  return (
    <section className="recent-sessions" aria-label="Recent sessions">
      <div className="recent-sessions-heading">
        <span>Recent</span>
        <div className="recent-sessions-heading-actions">
          {selectionMode && <span className="recent-selection-count">{selected.size} selected</span>}
          <button
            className="recent-sessions-more"
            type="button"
            aria-label="Recent session actions"
            aria-haspopup="menu"
            aria-expanded={headingMenuOpen}
            onClick={() => {
              setHeadingMenuOpen((current) => !current);
              setOpenMenuId(undefined);
            }}
          >
            <Ellipsis size={16} />
          </button>
          {headingMenuOpen && (
            <div className="recent-session-menu recent-session-bulk-menu" role="menu" aria-label="Recent session actions">
              <button type="button" role="menuitem" onClick={selectAll}>Select all</button>
              <button type="button" role="menuitem" onClick={selectNone}>Select none</button>
              <button
                className="danger"
                type="button"
                role="menuitem"
                disabled={!selected.size}
                onClick={deleteSelected}
              >
                <Trash2 size={15} />
                Delete selected
              </button>
              {selectionMode && (
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    setSelectionMode(false);
                    setSelected(new Set());
                    setHeadingMenuOpen(false);
                  }}
                >
                  Done selecting
                </button>
              )}
            </div>
          )}
        </div>
      </div>
      {loading && sessions.length === 0 && (
        <div className="recent-sessions-state" role="status"><LoaderCircle className="spin" size={13} /> Loading…</div>
      )}
      {!!error && <div className="recent-sessions-state error" role="alert">Unable to load recent sessions</div>}
      {!loading && !error && repo && visibleSessions.length === 0 && (
        <div className="recent-sessions-state">No recent sessions</div>
      )}
      {displayedSessions.map((session) => {
        const title = titleFor(session);
        const pinned = sessionUi[session.id]?.pinned === true;
        const checked = selected.has(session.id);
        return (
          <div className={`recent-session-item${openMenuId === session.id ? " menu-open" : ""}`} key={session.id}>
            {selectionMode && (
              <input
                className="recent-session-checkbox"
                type="checkbox"
                checked={checked}
                aria-label={`Select ${title}`}
                onChange={() => toggleSelection(session.id)}
              />
            )}
            {renamingSession === session.id ? (
              <input
                className="recent-session-rename"
                aria-label={`Rename ${title}`}
                value={renameText}
                autoFocus
                onChange={(event) => setRenameText(event.target.value)}
                onBlur={() => finishRename(session.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    finishRename(session.id);
                  } else if (event.key === "Escape") {
                    event.preventDefault();
                    setRenamingSession(undefined);
                    setRenameText("");
                  }
                }}
              />
            ) : (
              <button
                className="recent-session-row"
                type="button"
                onClick={() => {
                  if (selectionMode) toggleSelection(session.id);
                  else requestRuntimeResume(session.id, repo);
                }}
                title={title}
              >
                <span>{title}</span>
                <span className="recent-session-meta">
                  {pinned && <Pin className="recent-session-pinned" size={12} aria-label="Pinned" />}
                  <time dateTime={session.updatedAt}>{formatSessionAge(session.updatedAt)}</time>
                </span>
              </button>
            )}
            {!selectionMode && renamingSession !== session.id && (
              <button
                className="recent-session-more"
                type="button"
                aria-label={`Actions for ${title}`}
                aria-haspopup="menu"
                aria-expanded={openMenuId === session.id}
                onClick={() => {
                  setOpenMenuId((current) => current === session.id ? undefined : session.id);
                  setHeadingMenuOpen(false);
                }}
              >
                <Ellipsis size={16} />
              </button>
            )}
            {openMenuId === session.id && (
              <div className="recent-session-menu" role="menu" aria-label={`Actions for ${title}`}>
                <button type="button" role="menuitem" onClick={() => beginRename(session)}>
                  <Pencil size={15} />
                  Rename
                </button>
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    updateSessionUi(session.id, { pinned: !pinned });
                    setOpenMenuId(undefined);
                  }}
                >
                  <Pin size={15} />
                  {pinned ? "Unpin chat" : "Pin chat"}
                </button>
                <button type="button" role="menuitem" onClick={() => archiveSession(session.id)}>
                  <Archive size={15} />
                  Archive
                </button>
                <button className="danger" type="button" role="menuitem" onClick={() => deleteSession(session.id)}>
                  <Trash2 size={15} />
                  Delete
                </button>
              </div>
            )}
          </div>
        );
      })}
    </section>
  );
}
