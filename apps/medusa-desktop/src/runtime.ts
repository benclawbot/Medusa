import { invoke } from "@tauri-apps/api/core";

export type Effort = "low" | "medium" | "high" | "auto";
export type SubmitDisposition = "started" | "queued";

export interface RuntimeStartResponse {
  runtimeId: string;
  repo: string;
}

export interface SessionSummary {
  id: string;
  objective: string;
  createdAt: string;
  updatedAt: string;
  completed: boolean;
  waitingForUser: boolean;
  turn: number;
}

export interface SessionMessage {
  role: "user" | "assistant" | string;
  text: string;
}

export interface SessionDetail {
  summary: SessionSummary;
  messages: SessionMessage[];
}

export interface CommandSuggestion {
  name: string;
  usage: string;
  description: string;
}

export interface FileAttachment {
  kind: "file";
  path: string;
}

export interface ImageAttachment {
  kind: "image";
  name: string;
  dataUrl: string;
}

export interface TextAttachment {
  kind: "text";
  name: string;
  text: string;
}

export type DesktopAttachment = FileAttachment | ImageAttachment | TextAttachment;

export interface DesktopPromptDraft {
  text: string;
  attachments: DesktopAttachment[];
  revision: number;
}

export interface RuntimeActivity {
  id?: string;
  kind: "assistant" | "done" | "error" | "tool" | "verification";
  title: string;
  details: string[];
}

export interface PlanStep {
  title: string;
  status: "pending" | "inProgress" | "completed" | "failed";
}

export interface TimelineSnapshot {
  runtimeId?: string;
  plan: PlanStep[];
  activities: RuntimeActivity[];
  busy: boolean;
}

export interface QuestionOption {
  label: string;
  description: string;
}

export interface QuestionPrompt {
  header: string;
  question: string;
  options: QuestionOption[];
  multiSelect: boolean;
}

export type RuntimeEvent =
  | { type: "started" }
  | { type: "assistantText"; text: string }
  | { type: "activity"; activity: RuntimeActivity }
  | { type: "plan"; steps: PlanStep[] }
  | { type: "question"; prompts: QuestionPrompt[] }
  | {
      type: "usage";
      inputTokens: number;
      outputTokens: number;
      cacheReadInputTokens: number;
      cacheCreationInputTokens: number;
      modelElapsedMillis: number;
    }
  | { type: "progress"; turn: number }
  | {
      type: "settings";
      model: string;
      effort: string;
      planMode: boolean;
      credentialConfigured: boolean;
    }
  | { type: "notice"; title: string; details: string[] }
  | { type: "newSession" }
  | { type: "compacted"; message: string }
  | { type: "completed"; sessionId: string }
  | { type: "turnFinished" }
  | { type: "cancelled" }
  | { type: "failed"; message: string };

export interface DesktopMemory {
  id: string;
  memoryType: string;
  title: string;
  body: string;
  createdAt: string;
  updatedAt: string;
  scope: string;
  projectId?: string;
  sessionId?: string;
  status: string;
  confidenceMilli: number;
  validation: string;
  sources: string[];
  supersedes: string[];
  supersededBy: string[];
  tags: string[];
  expiresAt?: string;
  lastValidatedAt: string;
  successfulReuseCount: number;
  path: string;
}

export interface ModelConfiguration {
  provider: string;
  model: string;
  effort: Effort;
  apiKey?: string;
}

const pendingResumeKey = "medusa.desktop.resumeSession";
const emptyTimeline: TimelineSnapshot = { plan: [], activities: [], busy: false };
let timelineSnapshot: TimelineSnapshot = emptyTimeline;
const timelineListeners = new Set<() => void>();

function publishTimeline(next: TimelineSnapshot): void {
  timelineSnapshot = next;
  timelineListeners.forEach((listener) => listener());
}

function reduceTimeline(runtimeId: string, events: RuntimeEvent[]): void {
  let next = timelineSnapshot.runtimeId === runtimeId
    ? timelineSnapshot
    : { ...emptyTimeline, runtimeId };

  for (const event of events) {
    switch (event.type) {
      case "started":
        next = { ...next, busy: true };
        break;
      case "activity": {
        const activities = [...next.activities];
        const index = event.activity.id
          ? activities.findIndex((item) => item.id === event.activity.id)
          : -1;
        if (index >= 0) activities[index] = event.activity;
        else activities.push(event.activity);
        next = { ...next, activities };
        break;
      }
      case "plan":
        next = { ...next, plan: event.steps };
        break;
      case "question":
      case "completed":
      case "turnFinished":
      case "cancelled":
      case "failed":
        next = { ...next, busy: false };
        break;
      case "newSession":
        next = { ...emptyTimeline, runtimeId };
        break;
      default:
        break;
    }
  }

  if (next !== timelineSnapshot) publishTimeline(next);
}

export function getTimelineSnapshot(): TimelineSnapshot {
  return timelineSnapshot;
}

export function subscribeTimeline(listener: () => void): () => void {
  timelineListeners.add(listener);
  return () => timelineListeners.delete(listener);
}

export async function startRuntime(repo?: string): Promise<RuntimeStartResponse> {
  const pendingSession = window.localStorage.getItem(pendingResumeKey);
  const response = repo && pendingSession
    ? await invoke<RuntimeStartResponse>("runtime_resume", { repo, sessionId: pendingSession })
    : await invoke<RuntimeStartResponse>("runtime_start", repo ? { repo } : {});
  if (repo && pendingSession) window.localStorage.removeItem(pendingResumeKey);
  publishTimeline({ ...emptyTimeline, runtimeId: response.runtimeId });
  return response;
}

export function requestRuntimeResume(sessionId: string): void {
  window.localStorage.setItem(pendingResumeKey, sessionId);
}

export async function listRuntimeSessions(repo: string): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("runtime_list_sessions", { repo });
}

export async function readRuntimeSession(repo: string, sessionId: string): Promise<SessionDetail> {
  return invoke<SessionDetail>("runtime_read_session", { repo, sessionId });
}

export async function listRuntimeMemories(
  repo: string,
  query = "",
  includeInactive = false,
): Promise<DesktopMemory[]> {
  return invoke<DesktopMemory[]>("runtime_list_memories", { repo, query, includeInactive });
}

export async function closeRuntime(runtimeId: string): Promise<void> {
  await invoke("runtime_close", { runtimeId });
  if (timelineSnapshot.runtimeId === runtimeId) publishTimeline(emptyTimeline);
}

export async function submitRuntime(
  runtimeId: string,
  draft: DesktopPromptDraft,
): Promise<SubmitDisposition> {
  return invoke<SubmitDisposition>("runtime_submit", { runtimeId, draft });
}

export async function runRuntimeCommand(runtimeId: string, input: string): Promise<void> {
  await invoke("runtime_command", { runtimeId, input });
}

export async function commandSuggestions(
  runtimeId: string,
  input: string,
): Promise<CommandSuggestion[]> {
  return invoke<CommandSuggestion[]>("runtime_command_suggestions", { runtimeId, input });
}

export async function cancelRuntime(runtimeId: string): Promise<boolean> {
  return invoke<boolean>("runtime_cancel", { runtimeId });
}

export async function pollRuntime(runtimeId: string): Promise<RuntimeEvent[]> {
  const events = await invoke<RuntimeEvent[]>("runtime_poll", { runtimeId, maxEvents: 200 });
  reduceTimeline(runtimeId, events);
  return events;
}

export async function configureRuntime(
  runtimeId: string,
  configuration: ModelConfiguration,
): Promise<void> {
  await invoke("runtime_configure_model", { runtimeId, configuration });
}
