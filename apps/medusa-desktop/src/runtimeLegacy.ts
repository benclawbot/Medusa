import { convertFileSrc, invoke } from "@tauri-apps/api/core";

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

export interface WebArtifact {
  path: string;
  title: string;
}

/**
 * Keep the preview inside the Tauri webview instead of handing the artifact
 * off to the user's default browser. The native asset protocol serves the
 * validated runtime artifact and preserves relative CSS, images, and scripts.
 */
export function webArtifactPreviewUrl(path: string): string {
  return convertFileSrc(path);
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

/** Keep provider-private reasoning out of the user-visible transcript. */
export function visibleAssistantText(text: string): string {
  if (typeof text !== "string") return "";
  let visible = text.replace(
    /<\s*(?:think|thinking|analysis)\b[^>]*>[\s\S]*?<\s*\/\s*(?:think|thinking|analysis)\s*>/gi,
    "",
  );
  visible = visible.replace(/<\s*(?:think|thinking|analysis)\b[^>]*>[\s\S]*$/i, "");
  return visible.replace(/<\s*\/?\s*(?:think|thinking|analysis)\b[^>]*>/gi, "").trim();
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
      totalTokens: number;
      durationMs: number;
      tokensPerSecondMilli: number;
      estimatedCostMicrousd: number;
      provenance: string;
    }
  | { type: "progress"; turn: number }
  | {
      type: "settings";
      model: string;
      effort: string;
      verbosity: string;
      planMode: boolean;
      credentialConfigured: boolean;
    }
  | ({ type: "configurationChanged" } & ConfigurationChanged)
  // Details are optional at the IPC boundary because older runtimes and
  // partially recovered sessions may omit an empty details field.
  | { type: "notice"; title: string; details?: string[] }
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

interface RuntimeRecoveryState {
  snapshot?: RecoveryView;
  completion?: { auditPath: string };
  suppressed: boolean;
}

const pendingResumeKey = "medusa.desktop.resumeSession";
const emptyTimeline: TimelineSnapshot = { plan: [], activities: [], busy: false };
const MAX_RUNTIME_STATE_ENTRIES = 8;
const timelineSnapshots = new Map<string, TimelineSnapshot>();
const recoverySnapshots = new Map<string, RuntimeRecoveryState>();
const timelineListeners = new Set<() => void>();
const recoveryListeners = new Set<() => void>();
let activeRuntimeId: string | undefined;
let timelineSnapshot: TimelineSnapshot = emptyTimeline;
let recoverySnapshot: RecoveryView | undefined;
let recoveryCompletion: { auditPath: string } | undefined;

function pruneRuntimeMap<T>(map: Map<string, T>): void {
  while (map.size > MAX_RUNTIME_STATE_ENTRIES) {
    const candidate = [...map.keys()].find((runtimeId) => runtimeId !== activeRuntimeId);
    if (!candidate) return;
    map.delete(candidate);
  }
}

function rememberTimeline(runtimeId: string, next: TimelineSnapshot): void {
  timelineSnapshots.delete(runtimeId);
  timelineSnapshots.set(runtimeId, next);
  pruneRuntimeMap(timelineSnapshots);
  if (activeRuntimeId === runtimeId && timelineSnapshot !== next) {
    timelineSnapshot = next;
    timelineListeners.forEach((listener) => listener());
  }
}

function rememberRecovery(runtimeId: string, next: RuntimeRecoveryState): void {
  recoverySnapshots.delete(runtimeId);
  recoverySnapshots.set(runtimeId, next);
  pruneRuntimeMap(recoverySnapshots);
  if (activeRuntimeId === runtimeId) {
    recoverySnapshot = next.snapshot;
    recoveryCompletion = next.completion;
    recoveryListeners.forEach((listener) => listener());
  }
}

function activateRuntime(runtimeId: string): void {
  activeRuntimeId = runtimeId;
  const timeline = timelineSnapshots.get(runtimeId) ?? { ...emptyTimeline, runtimeId };
  timelineSnapshots.set(runtimeId, timeline);
  const recovery = recoverySnapshots.get(runtimeId) ?? { suppressed: false };
  recoverySnapshots.set(runtimeId, recovery);
  pruneRuntimeMap(timelineSnapshots);
  pruneRuntimeMap(recoverySnapshots);
  timelineSnapshot = timeline;
  recoverySnapshot = recovery.snapshot;
  recoveryCompletion = recovery.completion;
  timelineListeners.forEach((listener) => listener());
  recoveryListeners.forEach((listener) => listener());
}

function activityIsTerminal(activity: RuntimeActivity): boolean {
  return activity.kind === "done" || activity.kind === "error";
}

function reduceTimeline(runtimeId: string, events: RuntimeEvent[]): void {
  const previous = timelineSnapshots.get(runtimeId) ?? { ...emptyTimeline, runtimeId };
  let next = previous;
  let activities = next.activities;
  let activitiesDirty = false;
  let activityIndexes: Map<string, number> | undefined;

  const ensureActivityIndexes = (): Map<string, number> => {
    if (!activityIndexes) {
      activityIndexes = new Map(
        activities.flatMap((activity, index) => activity.id ? [[activity.id, index] as const] : []),
      );
    }
    return activityIndexes;
  };

  const replaceOrAppendActivity = (activity: RuntimeActivity): void => {
    if (!activity.id) {
      if (!activitiesDirty) {
        activities = [...activities];
        activitiesDirty = true;
      }
      activities.push(activity);
      return;
    }
    const indexes = ensureActivityIndexes();
    const index = indexes.get(activity.id);
    if (index !== undefined && activityIsTerminal(activities[index]) && !activityIsTerminal(activity)) {
      return;
    }
    if (!activitiesDirty) {
      activities = [...activities];
      activitiesDirty = true;
      activityIndexes = undefined;
    }
    const refreshedIndexes = ensureActivityIndexes();
    const refreshedIndex = refreshedIndexes.get(activity.id);
    if (refreshedIndex === undefined) {
      refreshedIndexes.set(activity.id, activities.length);
      activities.push(activity);
    } else {
      activities[refreshedIndex] = activity;
    }
  };

  for (const event of events) {
    switch (event.type) {
      case "started":
        next = { ...next, busy: true };
        break;
      case "activity": {
        replaceOrAppendActivity(event.activity);
        break;
      }
      case "team": {
        if (next.team && event.snapshot.sequence < next.team.sequence) break;
        const activeWorkerIds = new Set(
          event.snapshot.workers.map((worker) => `team:${worker.workerId}`),
        );
        const filteredActivities = activities.filter(
          (activity) =>
            !activity.id?.startsWith("team:") ||
            activeWorkerIds.has(activity.id),
        );
        if (filteredActivities.length !== activities.length) {
          activities = filteredActivities;
          activitiesDirty = true;
          activityIndexes = undefined;
        }
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
          replaceOrAppendActivity(activity);
        }
        next = { ...next, team: event.snapshot };
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
        activities = next.activities;
        activitiesDirty = false;
        activityIndexes = undefined;
        break;
      default:
        break;
    }
  }

  if (activitiesDirty) next = { ...next, activities };
  if (next !== previous || !timelineSnapshots.has(runtimeId)) rememberTimeline(runtimeId, next);
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

/** Hide the recovery overlay without pretending that its durable state was repaired. */
export function dismissRecovery(): void {
  if (!activeRuntimeId) return;
  const state = recoverySnapshots.get(activeRuntimeId) ?? { suppressed: false };
  rememberRecovery(activeRuntimeId, { ...state, suppressed: true, snapshot: undefined });
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
  timelineSnapshots.set(response.runtimeId, { ...emptyTimeline, runtimeId: response.runtimeId });
  recoverySnapshots.set(response.runtimeId, { suppressed: false });
  activateRuntime(response.runtimeId);
  return response;
}

export const RUNTIME_RESUME_EVENT = "medusa-runtime-resume";
export const REPO_CHANGED_EVENT = "medusa-repo-changed";

export function requestRuntimeResume(sessionId: string): void {
  window.localStorage.setItem(pendingResumeKey, sessionId);
  window.dispatchEvent(new CustomEvent<string>(RUNTIME_RESUME_EVENT, { detail: sessionId }));
}

export function publishRepoChanged(repo: string): void {
  const normalized = repo.trim();
  if (normalized) window.localStorage.setItem("medusa.desktop.repo", normalized);
  else window.localStorage.removeItem("medusa.desktop.repo");
  window.dispatchEvent(new CustomEvent<string>(REPO_CHANGED_EVENT, { detail: normalized }));
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
  timelineSnapshots.delete(runtimeId);
  recoverySnapshots.delete(runtimeId);
  if (activeRuntimeId === runtimeId) {
    activeRuntimeId = undefined;
    timelineSnapshot = emptyTimeline;
    recoverySnapshot = undefined;
    recoveryCompletion = undefined;
    timelineListeners.forEach((listener) => listener());
    recoveryListeners.forEach((listener) => listener());
  }
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

export async function findWebArtifact(runtimeId: string): Promise<WebArtifact | undefined> {
  const artifact = await invoke<WebArtifact | null>("runtime_find_web_artifact", { runtimeId });
  return artifact ?? undefined;
}

export async function openWebArtifact(runtimeId: string, path: string): Promise<void> {
  await invoke("runtime_open_web_artifact", { runtimeId, path });
}

export async function pollRuntime(runtimeId: string): Promise<RuntimeEvent[]> {
  const events = await invoke<RuntimeEvent[]>("runtime_poll", { runtimeId, maxEvents: 200 });
  reduceTimeline(runtimeId, events);
  let recovery = recoverySnapshots.get(runtimeId) ?? { suppressed: false };
  for (const event of events) {
    if (event.type === "recoveryAvailable" && !recovery.suppressed) {
      recovery = { ...recovery, snapshot: event.recovery, completion: undefined };
    }
    if (event.type === "recoveryCompleted") {
      recovery = { suppressed: true, snapshot: undefined, completion: { auditPath: event.auditPath } };
    }
    if (event.type === "completed" || event.type === "turnFinished") {
      recovery = { ...recovery, suppressed: true, snapshot: undefined, completion: undefined };
    }
    if (event.type === "failed" || event.type === "cancelled") {
      recovery = { ...recovery, suppressed: false };
    }
    if (event.type === "newSession") {
      recovery = { suppressed: false };
    }
  }
  rememberRecovery(runtimeId, recovery);
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
