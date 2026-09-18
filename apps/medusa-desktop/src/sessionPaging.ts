import { invoke } from "@tauri-apps/api/core";
import type { SessionMessage, SessionSummary } from "./runtime";

export interface SessionPage {
  sessions: SessionSummary[];
  nextCursor?: string;
}

export interface SessionMessagePage {
  summary: SessionSummary;
  messages: SessionMessage[];
  nextCursor?: string;
}

export async function listRuntimeSessionPage(
  repo: string,
  cursor?: string,
  limit = 24,
): Promise<SessionPage> {
  return invoke<SessionPage>("runtime_list_sessions_page", {
    repo,
    cursor: cursor ?? null,
    limit,
  });
}

export async function readRuntimeSessionPage(
  repo: string,
  sessionId: string,
  cursor?: string,
  limit = 100,
): Promise<SessionMessagePage> {
  return invoke<SessionMessagePage>("runtime_read_session_page", {
    repo,
    sessionId,
    cursor: cursor ?? null,
    limit,
  });
}


export async function renameRuntimeSession(
  repo: string,
  sessionId: string,
  title: string,
): Promise<void> {
  await invoke("runtime_rename_session", { repo, sessionId, title });
}

export async function setRuntimeSessionPinned(
  repo: string,
  sessionId: string,
  pinned: boolean,
): Promise<void> {
  await invoke("runtime_set_session_pinned", { repo, sessionId, pinned });
}

export async function archiveRuntimeSession(repo: string, sessionId: string): Promise<void> {
  await invoke("runtime_archive_session", { repo, sessionId });
}

export async function deleteRuntimeSessions(repo: string, sessionIds: string[]): Promise<void> {
  await invoke("runtime_delete_sessions", { repo, sessionIds });
}
