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
