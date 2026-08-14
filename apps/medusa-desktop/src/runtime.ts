import { invoke } from "@tauri-apps/api/core";

export type Effort = "low" | "medium" | "high" | "auto";
export type SubmitDisposition = "started" | "queued";

export type RecoveryHealth = "ready" | "needsConfirmation" | "blocked" | "corrupt" | "Ready" | "NeedsConfirmation" | "Blocked" | "Corrupt";
export type VerificationState = "verified" | "failed" | "incomplete" | "unknown" | "Verified" | "Failed" | "Incomplete" | "Unknown";
export type RecoveryOperation = "inspect" | "resume" | "restoreCheckpoint" | "retryVerification" | "abandon";

export interface RecoveryCheckpoint {
  id: string;
  sequence: number;
  createdAtUnixMs: number;
  taskStep: string;
  reason: string;
  repositoryFingerprint: string;
  verification: VerificationState;
  provenance: string;
  integrityVerified: boolean;
}

export interface RecoveryPreviewFile {
  path: string;
  kind: string;
  wouldOverwriteUncommittedWork: boolean;
}

export interface RecoveryPreview {
  checkpointId: string;
  files: RecoveryPreviewFile[];
  unresolvedRisks: string[];
  repositoryMatchesCheckpointBase: boolean;
}

export interface RecoveryActionAvailability {
  operation: RecoveryOperation;
  enabled: boolean;
  requiresConfirmation: boolean;
  reason: string;
}

export interface RecoveryView {
  sessionId: string;
  health: RecoveryHealth;
  lastDurableStep: string;
  interruptedOperation?: string;
  currentRepositoryFingerprint: string;
  verification: VerificationState;
  approvalsMustBeReestablished: boolean;
  containmentMustBeReestablished: boolean;
  checkpoints: RecoveryCheckpoint[];
  selectedPreview?: RecoveryPreview;
  actions: RecoveryActionAvailability[];
  warnings: string[];
}

export interface RuntimeStartResponse {
  runtimeId: string;
  repo: string;
}

export interface SharedConfiguration {
  revision: number;
  activeProfile: string;
  connection: string;
  provider: string;
  model: string;
  effort: Effort;
  auth: string;
  baseUrl?: string;
  configured: boolean;
  credentialConfigured: boolean;
}

export interface ConfigurationChanged {
  revision: number;
  activeProfile: string;
  changedKeys: string[];
  origin: "cli" | "tui" | "desktop" | "system" | string;
  applyTiming: "immediate" | "next-session" | "restart-required" | string;
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
  mediaType?: string;
  sizeBytes?: number;
  width?: number;
  height?: number;
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
  kind: "assistant" | "done" | "error" | "tool" | "progress" | "verification";
  title: string;
  details: string[];
}

export interface PlanStep {
  title: string;
  status: "pending" | "inProgress" | "completed" | "failed";
}

export interface TeamWorkerSnapshot {
  workerId: string;
  role: string;
  taskId: string;
  lifecycle: "pending" | "running" | "retrying" | "cancellation_requested" | "completed" | "failed" | "integrated";
  sessionId?: string;
  turn: number;
  lastUpdate: string;
  queuedInstructions: number;
}

export interface TeamSnapshot {
  executionId?: string;
  active: boolean;
  shutdownRequested: boolean;
  sequence: number;
  workers: TeamWorkerSnapshot[];
}

export interface TimelineSnapshot {
  runtimeId?: string;
  plan: PlanStep[];
  activities: RuntimeActivity[];
  team?: TeamSnapshot;
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
  | { type: "recoveryAvailable"; recovery: RecoveryView }
  | { type: "recoveryCompleted"; record: unknown; auditPath: string }
  | { type: "started" }
  | { type: "assistantText"; text: string }
  | { type: "activity"; activity: RuntimeActivity }
  | { type: "team"; snapshot: TeamSnapshot }
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
  | ({ type: "configurationChanged" } & ConfigurationChanged)
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
  expectedRevision: number;
  apiKey?: string;
  baseUrl?: string;
}

const pendingResumeKey = "medusa.desktop.resumeSession";
const emptyTimeline: TimelineSnapshot = { plan: [], activities: [], busy: false };
let timelineSnapshot: TimelineSnapshot = emptyTimeline;
const timelineListeners = new Set<() => void>();
const recoveryListeners = new Set<() => void>();
let recoverySnapshot: RecoveryView | undefined;
let recoveryCompletion: { auditPath: string } | undefined;

function publishRecovery(recovery: RecoveryView | undefined, completion?: { auditPath: string }): void {
  recoverySnapshot = recovery;
  recoveryCompletion = completion;
  recoveryListeners.forEach((listener) => listener());
}

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
      case "team": {
        const activeWorkerIds = new Set(
          event.snapshot.workers.map((worker) => `team:${worker.workerId}`),
        );
        const activities = next.activities.filter(
          (activity) =>
            !activity.id?.startsWith("team:") ||
            activeWorkerIds.has(activity.id),
        );
        for (const worker of event.snapshot.workers) {
          const activity: RuntimeActivity = {
            id: `team:${worker.workerId}`,
            kind:
              worker.lifecycle === "failed"
                ? "error"
                : worker.lifecycle === "completed" ||
                    worker.lifecycle === "integrated"
                  ? "done"
                  : "progress",
            title: `${worker.workerId} · ${worker.taskId} · ${worker.lifecycle}`,
            details: [
              `role ${worker.role}`,
              `turn ${worker.turn}`,
              `session ${worker.sessionId ?? "pending"}`,
              worker.lastUpdate,
            ],
          };
          const index = activities.findIndex(
            (item) => item.id === activity.id,
          );
          if (index >= 0) activities[index] = activity;
          else activities.push(activity);
        }
        next = { ...next, team: event.snapshot, activities };
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

export function getRecoverySnapshot(): RecoveryView | undefined {
  return recoverySnapshot;
}

export function getRecoveryCompletion(): { auditPath: string } | undefined {
  return recoveryCompletion;
}

export function subscribeRecovery(listener: () => void): () => void {
  recoveryListeners.add(listener);
  return () => recoveryListeners.delete(listener);
}

export async function loadSharedConfiguration(): Promise<SharedConfiguration> {
  return invoke<SharedConfiguration>("desktop_shared_configuration");
}

export async function startRuntime(repo?: string): Promise<RuntimeStartResponse> {
  const pendingSession = window.localStorage.getItem(pendingResumeKey);
  const response = repo && pendingSession
    ? await invoke<RuntimeStartResponse>("runtime_resume", { repo, sessionId: pendingSession })
    : await invoke<RuntimeStartResponse>("runtime_start", repo ? { repo } : {});
  if (repo && pendingSession) window.localStorage.removeItem(pendingResumeKey);
  publishTimeline({ ...emptyTimeline, runtimeId: response.runtimeId });
  publishRecovery(undefined);
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
  publishRecovery(undefined);
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
  for (const event of events) {
    if (event.type === "recoveryAvailable") publishRecovery(event.recovery);
    if (event.type === "recoveryCompleted") publishRecovery(undefined, { auditPath: event.auditPath });
    if (event.type === "newSession") publishRecovery(undefined);
  }
  return events;
}

export async function configureRuntime(
  runtimeId: string,
  configuration: ModelConfiguration,
): Promise<ConfigurationChanged | undefined> {
  return invoke<ConfigurationChanged | undefined>("runtime_configure_model", {
    runtimeId,
    configuration,
  });
}

export async function performRecoveryAction(
  runtimeId: string,
  recovery: RecoveryView,
  operation: RecoveryOperation,
  checkpointId?: string,
  confirmedDestructiveEffects = false,
): Promise<void> {
  const checkpoint = checkpointId
    ? recovery.checkpoints.find((item) => item.id === checkpointId)
    : undefined;
  const preview = recovery.selectedPreview?.checkpointId === checkpointId
    ? recovery.selectedPreview
    : undefined;

  await invoke("runtime_recovery_action", {
    runtimeId,
    request: {
      recovery,
      operation,
      checkpointId,
      confirmedDestructiveEffects,
      repositoryFingerprintBefore: recovery.currentRepositoryFingerprint,
      checkpointIntegrityVerified: checkpoint?.integrityVerified ?? operation !== "restoreCheckpoint",
      repositoryPreconditionsVerified: preview?.repositoryMatchesCheckpointBase ?? operation !== "restoreCheckpoint",
      conflictingUncommittedPaths: preview?.files
        .filter((file) => file.wouldOverwriteUncommittedWork)
        .map((file) => file.path) ?? [],
      unresolvedRisks: preview?.unresolvedRisks ?? [],
    },
  });
}
