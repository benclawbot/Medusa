import {
  Activity,
  BarChart3,
  Bot,
  CheckCircle2,
  Check,
  ChevronDown,
  ChevronRight,
  Circle,
  Copy,
  Download,
  ExternalLink,
  FilePlus2,
  FolderOpen,
  Gauge,
  GraduationCap,
  ImagePlus,
  Info,
  ListChecks,
  Maximize2,
  MessageSquare,
  OctagonX,
  PanelLeftClose,
  PanelLeftOpen,
  PanelRightClose,
  PanelRightOpen,
  Play,
  Plus,
  Send,
  Settings,
  ShieldCheck,
  Square,
  X,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import packageMetadata from "../package.json";
import { ApprovalCard } from "./ApprovalCard";
import { appendBounded } from "./boundedHistory";
import { RecoveryDock } from "./RecoveryDock";
import { DesktopOnboarding } from "./DesktopOnboarding";
import { requestDesktopTool, type DesktopTool } from "./desktop-tools";
import { MarkdownMessage } from "./MarkdownMessage";
import { SessionDock } from "./SessionDock";
import { useDialogFocus } from "./useDialogFocus";
import { toUserError } from "./errorPresentation";
import "./approval-card.css";
import "./ux-polish.css";
import {
  loadProviderCatalog,
  ensureBrowserOauth,
  profileModelCapabilityState,
  startBrowserOauth,
  type ProviderCatalogEntry,
} from "./providerCatalog";
import {
  cancelRuntime,
  commandSuggestions,
  closeRuntime,
  configureRuntime,
  dismissRecovery,
  findWebArtifact,
  openWebArtifact,
  loadSharedConfiguration,
  pollRuntime,
  publishRepoChanged,
  RUNTIME_RESUME_EVENT,
  restoreRuntime,
  resumeRuntime,
  runRuntimeCommand,
  startRuntime,
  submitRuntime,
  type DesktopAttachment,
  type CommandSuggestion,
  type Effort,
  type PlanStep,
  type QuestionPrompt,
  type RuntimeActivity,
  type RuntimeEvent,
  type SharedConfiguration,
  type WebArtifact,
  webArtifactBrowserUrl,
  webArtifactPreviewUrl,
  visibleAssistantText,
} from "./runtime";

interface ConversationMessage {
  id: number;
  role: "user" | "assistant";
  text: string;
  createdAt: number;
  attachments?: DesktopAttachment[];
  queued?: boolean;
  failed?: boolean;
}

interface WorkLogEntry {
  id: string;
  kind: "input" | "activity" | "status";
  text: string;
  timestamp: number;
  status?: string;
  details?: string[];
  activityId?: string;
}

interface UsageState {
  input: number;
  output: number;
  cached: number;
  cacheWrite: number;
  total: number;
  elapsed: number;
}

type Verbosity = "off" | "new" | "all" | "verbose";

function parseVerbosity(value: unknown): Verbosity {
  return value === "off" || value === "new" || value === "verbose" ? value : "all";
}

interface SettingsState {
  model: string;
  effort: string;
  verbosity: Verbosity;
  planMode: boolean;
  credentialConfigured: boolean;
}

type SidePanelView = "work" | "preview" | "details";

const emptyUsage: UsageState = { input: 0, output: 0, cached: 0, cacheWrite: 0, total: 0, elapsed: 0 };
let messageCounter = 0;
const nextMessageId = () => ++messageCounter;
let workEntryCounter = 0;
const nextWorkEntryId = () => `work-${++workEntryCounter}`;
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
const MAX_COMPOSER_HEIGHT = 160;
const MAX_MESSAGE_HISTORY = 2000;
const MAX_WORK_LOG_ENTRIES = 1000;
const MAX_ACTIVITY_ENTRIES = 1000;
const SUPPORTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/webp", "image/gif"]);
const EFFORT_ORDER: Effort[] = ["auto", "low", "medium", "high"];
const timestampFormatter = new Intl.DateTimeFormat(undefined, {
  hour: "numeric",
  minute: "2-digit",
  second: "2-digit",
});

function effortOptionsForModel(provider: ProviderCatalogEntry | undefined, model: string): Effort[] {
  const metadata = provider?.models?.find((candidate) => candidate.id === model);
  if (!metadata) return EFFORT_ORDER;
  const supported = new Set(metadata.capabilities.reasoning_effort_levels.map((value) => value.toLowerCase()));
  const options = EFFORT_ORDER.filter((value) => value === "auto" || supported.has(value));
  return options.length ? options : ["auto"];
}

function effortLabel(value: Effort): string {
  return value === "auto" ? "Auto" : value[0].toUpperCase() + value.slice(1);
}

function formatBytes(bytes?: number): string {
  if (bytes === undefined) return "unknown size";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function isMacPlatform(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform);
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function formatTimestamp(timestamp: number): string {
  return timestampFormatter.format(timestamp);
}

async function copyTextToClipboard(text: string): Promise<void> {
  // Tauri serves a secure context, so the async clipboard API is always present.
  if (!navigator.clipboard?.writeText) {
    throw new Error("The clipboard is unavailable.");
  }
  await navigator.clipboard.writeText(text);
}

function readImage(file: File): Promise<DesktopAttachment> {
  return new Promise((resolve, reject) => {
    if (!SUPPORTED_IMAGE_TYPES.has(file.type)) {
      reject(new Error(`Unsupported image type ${file.type || "unknown"}. Use PNG, JPEG, WebP, or GIF.`));
      return;
    }
    if (file.size > MAX_IMAGE_BYTES) {
      reject(new Error(`${file.name || "Image"} is ${formatBytes(file.size)}; the maximum is 20 MB.`));
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== "string") {
        reject(new Error("The image could not be read."));
        return;
      }
      const image = new Image();
      image.onload = () => resolve({
        kind: "image",
        name: file.name || "pasted-image.png",
        dataUrl: reader.result as string,
        mediaType: file.type,
        sizeBytes: file.size,
        width: image.naturalWidth,
        height: image.naturalHeight,
      });
      image.onerror = () => reject(new Error(`${file.name || "Image"} could not be decoded.`));
      image.src = reader.result;
    };
    reader.onerror = () => reject(new Error("The image could not be read."));
    reader.readAsDataURL(file);
  });
}

function planIcon(status: PlanStep["status"]) {
  if (status === "completed") return <CheckCircle2 size={15} />;
  if (status === "failed") return <OctagonX size={15} />;
  if (status === "inProgress") return <Play size={14} />;
  return <Circle size={13} />;
}

async function configureStartedRuntime(
  started: Awaited<ReturnType<typeof startRuntime>>,
  configuration: {
    provider: string;
    model: string;
    effort: Effort;
    expectedRevision: number;
  },
  options: { preserveRuntimeOnDependencyFailure?: boolean } = {},
): Promise<Awaited<ReturnType<typeof startRuntime>>> {
  try {
    await configureRuntime(started.runtimeId, configuration);
    return started;
  } catch (cause) {
    const transientDependencyFailure = String(cause).includes("dependency unavailable: daemon");
    if (!options.preserveRuntimeOnDependencyFailure || !transientDependencyFailure) {
      try {
        await closeRuntime(started.runtimeId);
      } catch (cleanupCause) {
        throw new Error(
          `Runtime configuration failed (${String(cause)}); cleanup also failed (${String(cleanupCause)}).`,
        );
      }
    }
    throw cause;
  }
}

function finishActivities(
  current: RuntimeActivity[],
  kind: "done" | "error",
  detail: string,
): RuntimeActivity[] {
  return current.map((item) => {
    if (item.kind === "done" || item.kind === "error") return item;
    return { ...item, kind, details: [...(item.details ?? []), detail] };
  });
}

type TurnSummaryStatus = "completed" | "failed" | "cancelled";

interface TurnSummaryState {
  status: TurnSummaryStatus;
  elapsedSeconds: number;
  error?: string;
}

function formatWorkedDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m ${remainingSeconds}s`;
}

function activityStatusClass(entry: WorkLogEntry): string {
  return entry.status === "Done" ? "done" : entry.status === "Error" ? "error" : "";
}

function LiveActivityTrail({
  entries,
  verboseDetails,
  elapsedSeconds,
  activeEntry,
}: {
  entries: WorkLogEntry[];
  verboseDetails: boolean;
  elapsedSeconds: number;
  activeEntry?: WorkLogEntry;
}) {
  return (
    <section className="activity-summary live-activity-trail" aria-label="Actions in progress">
      <div className="activity-summary-heading">
        <span><Activity size={15} aria-hidden="true" /> Working for {formatWorkedDuration(elapsedSeconds)}</span>
        <small>{activeEntry?.text ?? "Starting turn…"}</small>
      </div>
      {entries.map((entry) => (
        <details className={`activity-row ${activityStatusClass(entry)}`} key={entry.id} open={entry.status === "Working" || verboseDetails || undefined}>
          <summary>
            <span aria-hidden="true"><Activity size={14} /></span>
            <strong>{entry.text}</strong>
            <small>{entry.status ?? "Working"}</small>
          </summary>
          {!!entry.details?.length && (
            <div className="activity-details">
              {entry.details.map((detail, index) => <p key={`${entry.id}-${index}`}>{detail}</p>)}
            </div>
          )}
        </details>
      ))}
    </section>
  );
}

function FinalTurnSummary({
  summary,
  activities,
  artifact,
  onPreview,
  onOpenExternally,
}: {
  summary: TurnSummaryState;
  activities: WorkLogEntry[];
  artifact?: WebArtifact;
  onPreview: () => void;
  onOpenExternally: () => void;
}) {
  const statusLabel = summary.status === "completed"
    ? "Turn completed"
    : summary.status === "cancelled"
      ? "Turn cancelled"
      : "Turn failed";
  const statusIcon = summary.status === "completed"
    ? <CheckCircle2 size={16} aria-hidden="true" />
    : <OctagonX size={16} aria-hidden="true" />;

  return (
    <section className={`turn-summary-card ${summary.status}`} aria-label="Turn summary">
      <div className="turn-summary-heading">
        <span className="turn-summary-worked">Worked for {formatWorkedDuration(summary.elapsedSeconds)}</span>
        <ChevronRight size={17} aria-hidden="true" />
      </div>
      <div className="turn-summary-status">
        <span className="turn-summary-status-icon">{statusIcon}</span>
        <strong>{statusLabel}</strong>
      </div>
      {!!summary.error && <p className="turn-summary-error">{summary.error}</p>}

      {!!activities.length && (
        <details className="turn-summary-details">
          <summary>Show execution details ({activities.length})</summary>
          <div className="turn-summary-activity-list">
            {activities.map((entry) => (
              <details className={`activity-row ${activityStatusClass(entry)}`} key={entry.id}>
                <summary>
                  <span aria-hidden="true"><Activity size={14} /></span>
                  <strong>{entry.text}</strong>
                  <small>{entry.status ?? "Recorded"}</small>
                </summary>
                {!!entry.details?.length && (
                  <div className="activity-details">
                    {entry.details.map((detail, index) => <p key={`${entry.id}-${index}`}>{detail}</p>)}
                  </div>
                )}
              </details>
            ))}
          </div>
        </details>
      )}

      {artifact && (
        <article className="turn-artifact-card">
          <div className="turn-artifact-heading">
            <div className="turn-artifact-icon" aria-hidden="true"><FilePlus2 size={18} /></div>
            <div>
              <a
                className="turn-artifact-link"
                href={webArtifactBrowserUrl(artifact.path)}
                target="_blank"
                rel="noreferrer"
                aria-label={`Open ${artifact.title} in your default browser`}
                onClick={(event) => {
                  event.preventDefault();
                  onOpenExternally();
                }}
                title="Open the built page in your default browser"
              >
                {artifact.title} <ExternalLink size={13} aria-hidden="true" />
              </a>
              <small>Website · {basename(artifact.path)}</small>
            </div>
          </div>
          <div className="turn-artifact-actions">
            <a
              className="turn-artifact-download"
              href={webArtifactPreviewUrl(artifact.path)}
              download={basename(artifact.path)}
              aria-label={`Download ${artifact.title}`}
              title="Download artifact"
            >
              <Download size={17} aria-hidden="true" />
            </a>
            <details className="turn-artifact-open-menu">
              <summary>Open in <ChevronDown size={15} aria-hidden="true" /></summary>
              <div role="menu">
                <button type="button" role="menuitem" onClick={onPreview}>Preview in Medusa</button>
                <button type="button" role="menuitem" onClick={onOpenExternally}><ExternalLink size={14} aria-hidden="true" /> Default browser</button>
              </div>
            </details>
          </div>
        </article>
      )}
    </section>
  );
}

interface AppProps {
  settingsSlot?: React.ReactNode;
  composerSlot?: React.ReactNode;
  composerToolsSlot?: React.ReactNode;
}

const DESKTOP_DRAFT_VERSION = 1;
const DESKTOP_DRAFT_LIMIT_BYTES = 512_000;

type PersistedDraftAttachment =
  | Extract<DesktopAttachment, { kind: "file" }>
  | Extract<DesktopAttachment, { kind: "text" }>;

interface PersistedDesktopDraft {
  version: typeof DESKTOP_DRAFT_VERSION;
  text: string;
  attachments: PersistedDraftAttachment[];
}

function desktopDraftKey(repo: string): string {
  const scope = repo.trim() || "__general__";
  return `medusa.desktop.draft.v${DESKTOP_DRAFT_VERSION}:${encodeURIComponent(scope)}`;
}

function loadDesktopDraft(repo: string): PersistedDesktopDraft | undefined {
  try {
    const raw = window.localStorage.getItem(desktopDraftKey(repo));
    if (!raw || raw.length > DESKTOP_DRAFT_LIMIT_BYTES) return undefined;
    const parsed = JSON.parse(raw) as Partial<PersistedDesktopDraft>;
    if (parsed.version !== DESKTOP_DRAFT_VERSION || typeof parsed.text !== "string" || !Array.isArray(parsed.attachments)) return undefined;
    const attachments = parsed.attachments.filter((attachment): attachment is PersistedDraftAttachment => {
      if (!attachment || typeof attachment !== "object" || !("kind" in attachment)) return false;
      return attachment.kind === "file" && typeof attachment.path === "string"
        || attachment.kind === "text" && typeof attachment.name === "string" && typeof attachment.text === "string";
    });
    return { version: DESKTOP_DRAFT_VERSION, text: parsed.text, attachments };
  } catch {
    return undefined;
  }
}

function persistDesktopDraft(repo: string, text: string, attachments: DesktopAttachment[]): void {
  const persisted: PersistedDesktopDraft = {
    version: DESKTOP_DRAFT_VERSION,
    text,
    // Image payloads are intentionally excluded: data URLs contain private content and can
    // exceed storage quotas. The current send still retains them in memory for this turn.
    attachments: attachments.filter((attachment): attachment is PersistedDraftAttachment => attachment.kind !== "image"),
  };
  if (!persisted.text.trim() && persisted.attachments.length === 0) {
    window.localStorage.removeItem(desktopDraftKey(repo));
    return;
  }
  const serialized = JSON.stringify(persisted);
  if (serialized.length <= DESKTOP_DRAFT_LIMIT_BYTES) {
    window.localStorage.setItem(desktopDraftKey(repo), serialized);
  }
}

function clearDesktopDraft(repo: string): void {
  window.localStorage.removeItem(desktopDraftKey(repo));
}

export function App({ settingsSlot, composerSlot, composerToolsSlot }: AppProps = {}) {
  const [runtimeId, setRuntimeId] = useState<string>();
  const [repo, setRepo] = useState("");
  // Keep the active project synchronously available to event handlers. React state
  // updates are scheduled, so a resume request can arrive between startRuntime()
  // resolving and the render that publishes its repo.
  const activeRepoRef = useRef("");
  const setActiveRepo = useCallback((nextRepo: string) => {
    activeRepoRef.current = nextRepo;
    setRepo(nextRepo);
  }, []);
  const [messages, setMessages] = useState<ConversationMessage[]>([]);
  const [activities, setActivities] = useState<RuntimeActivity[]>([]);
  const [workLog, setWorkLog] = useState<WorkLogEntry[]>([]);
  const [plan, setPlan] = useState<PlanStep[]>([]);
  const [questions, setQuestions] = useState<QuestionPrompt[]>([]);
  const [usage, setUsage] = useState<UsageState>(emptyUsage);
  const [settings, setSettings] = useState<SettingsState>({
    model: "not connected",
    effort: "effort:auto",
    verbosity: "all",
    planMode: false,
    credentialConfigured: false,
  });
  const [prompt, setPrompt] = useState("");
  const [lastRequest, setLastRequest] = useState<{ text: string; attachments: DesktopAttachment[] }>();
  const [slashSuggestions, setSlashSuggestions] = useState<CommandSuggestion[]>([]);
  const [slashSelection, setSlashSelection] = useState(0);
  const slashOptionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [attachments, setAttachments] = useState<DesktopAttachment[]>([]);
  const [previewImage, setPreviewImage] = useState<Extract<DesktopAttachment, { kind: "image" }>>();
  const [draggingImage, setDraggingImage] = useState(false);
  const [busy, setBusy] = useState(false);
  const [transcriptLimit, setTranscriptLimit] = useState(120);
  const [copiedMessageId, setCopiedMessageId] = useState<number>();
  const [pendingSubmit, setPendingSubmit] = useState(false);
  const [turn, setTurn] = useState(0);
  const [error, setError] = useState<string>();
  const [provider, setProvider] = useState("");
  const [providerCatalog, setProviderCatalog] = useState<ProviderCatalogEntry[]>([]);
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState<Effort>("medium");
  const [sharedConfiguration, setSharedConfiguration] = useState<SharedConfiguration>();
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [authenticating, setAuthenticating] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  const [oauthAuthenticatedProvider, setOauthAuthenticatedProvider] = useState<string>();
  const [composerSelectorOpen, setComposerSelectorOpen] = useState(false);
  const [activePanel, setActivePanel] = useState<"chat" | "plan" | "settings">("chat");
  const [railCollapsed, setRailCollapsed] = useState(false);
  const [sidePanelView, setSidePanelView] = useState<SidePanelView | undefined>("work");
  const [sidePanelHost, setSidePanelHost] = useState<HTMLDivElement | null>(null);
  const [sidePanelWidth, setSidePanelWidth] = useState(320);
  const [sidePanelResizing, setSidePanelResizing] = useState(false);
  const sidePanelResizeStart = useRef<{ x: number; width: number }>();
  const [webArtifact, setWebArtifact] = useState<WebArtifact>();
  const [partialResult, setPartialResult] = useState(false);
  const [turnSummary, setTurnSummary] = useState<TurnSummaryState>();
  const [liveElapsedSeconds, setLiveElapsedSeconds] = useState(0);
  const turnStartedAt = useRef<number>();
  const assistantResponseInTurn = useRef(false);
  const assistantStream = useRef<{ id: number; raw: string; text: string; createdAt: number }>();
  const assistantDeltaFrame = useRef<number>();
  // A terminal event can be replayed or arrive in the same poll batch as the final
  // assistant delta. Keep the decision idempotent and flush the stream before
  // deciding whether the runtime returned a summary.
  const terminalEventHandled = useRef(false);
  const lastTransportError = useRef<string>();
  const transportFailureCount = useRef(0);
  const transportErrorVisible = useRef(false);
  const pollBusy = useRef(false);
  const runtimeGeneration = useRef(0);
  const submitInFlight = useRef(false);
  const failedMessage = useRef<{ id: number; text: string; attachments: DesktopAttachment[] }>();
  const hydratedDraftScope = useRef<string>();
  const hydratedDraftRepo = useRef<string>();
  const resumeInFlight = useRef(false);
  const runtimeTransitionInFlight = useRef(false);
  const composerRevision = useRef(0);
  const providerRequestId = useRef(0);
  const authenticationRequestId = useRef(0);
  const wakePoll = useRef<(() => void) | undefined>();
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const transcriptRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const imageInputRef = useRef<HTMLInputElement>(null);
  const composerSelectorRef = useRef<HTMLDivElement>(null);
  const previewDialogRef = useRef<HTMLDivElement>(null);
  const closeComposerSelector = useCallback(() => setComposerSelectorOpen(false), []);
  const closePreviewImage = useCallback(() => setPreviewImage(undefined), []);
  const markComposerEdited = useCallback(() => {
    composerRevision.current += 1;
  }, []);
  type ComposerSnapshot = { revision: number; repo: string };
  const composerSnapshot = (): ComposerSnapshot => ({
    revision: composerRevision.current,
    repo,
  });
  const isCurrentComposerSnapshot = (snapshot: ComposerSnapshot): boolean => (
    composerRevision.current === snapshot.revision
  );
  const isActiveComposerScope = (snapshot: ComposerSnapshot): boolean => (
    activeRepoRef.current === snapshot.repo
  );
  const clearAcceptedComposer = (snapshot: ComposerSnapshot): void => {
    if (!isCurrentComposerSnapshot(snapshot)) return;
    // Clear the submitted scope even when the user has since switched projects;
    // the key is scoped to the submitted project and cannot remove the new
    // project's draft.
    clearDesktopDraft(snapshot.repo);
    if (isActiveComposerScope(snapshot)) {
      setPrompt("");
      setAttachments([]);
    }
  };

  useEffect(() => {
    if (hydratedDraftRepo.current !== undefined && hydratedDraftRepo.current !== repo) {
      persistDesktopDraft(hydratedDraftRepo.current, prompt, attachments);
    }
    const draft = loadDesktopDraft(repo);
    hydratedDraftScope.current = desktopDraftKey(repo);
    hydratedDraftRepo.current = repo;
    setPrompt(draft?.text ?? "");
    setAttachments(draft?.attachments ?? []);
  }, [repo]);

  useEffect(() => {
    const scope = desktopDraftKey(repo);
    if (hydratedDraftScope.current !== scope) return;
    if (submitInFlight.current) return;
    const timer = window.setTimeout(() => persistDesktopDraft(repo, prompt, attachments), 200);
    return () => window.clearTimeout(timer);
  }, [attachments, pendingSubmit, prompt, repo]);

  useDialogFocus(composerSelectorOpen, composerSelectorRef, closeComposerSelector);
  useDialogFocus(Boolean(previewImage), previewDialogRef, closePreviewImage);

  const resizeComposer = useCallback(() => {
    const composer = composerRef.current;
    if (!composer) return;

    composer.style.height = "auto";
    if (composer.scrollHeight > 0) {
      composer.style.height = `${Math.min(composer.scrollHeight, MAX_COMPOSER_HEIGHT)}px`;
      composer.style.overflowY = composer.scrollHeight > MAX_COMPOSER_HEIGHT ? "auto" : "hidden";
    }
  }, []);

  useLayoutEffect(() => {
    resizeComposer();
  }, [prompt, resizeComposer]);

  useEffect(() => {
    if (!runtimeId || activePanel !== "chat") return;
    const frame = window.requestAnimationFrame(() => composerRef.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, [runtimeId, activePanel]);

  const appendWorkLog = useCallback((entry: Omit<WorkLogEntry, "id" | "timestamp"> & { timestamp?: number }) => {
    const id = nextWorkEntryId();
    const timestamp = entry.timestamp ?? Date.now();
    setWorkLog((current) => appendBounded(current, {
        ...entry,
        id,
        timestamp,
      }, MAX_WORK_LOG_ENTRIES));
  }, []);

  const flushAssistantStream = useCallback(() => {
    if (assistantDeltaFrame.current !== undefined) {
      window.cancelAnimationFrame(assistantDeltaFrame.current);
      assistantDeltaFrame.current = undefined;
    }
    const stream = assistantStream.current;
    if (!stream?.raw) return;
    const text = visibleAssistantText(stream.raw);
    stream.text = text;
    if (!text) return;
    assistantResponseInTurn.current = true;
    setMessages((current) => {
      const index = current.findIndex((message) => message.id === stream.id);
      if (index < 0) {
        return appendBounded(current, { id: stream.id, role: "assistant", text, createdAt: stream.createdAt }, MAX_MESSAGE_HISTORY);
      }
      const next = [...current];
      next[index] = { ...next[index], text };
      return next;
    });
  }, []);

  const appendAssistantMessage = useCallback((value: string) => {
    flushAssistantStream();
    const text = visibleAssistantText(value);
    if (!text) return;
    assistantResponseInTurn.current = true;
    const stream = assistantStream.current;
    if (stream && (text === stream.text || text.startsWith(stream.text))) {
      stream.text = text;
      setMessages((current) => current.map((message) => (
        message.id === stream.id ? { ...message, text } : message
      )));
      return;
    }
    const message = {
      id: nextMessageId(),
      role: "assistant" as const,
      text,
      createdAt: Date.now(),
    };
    setMessages((current) => appendBounded(current, message, MAX_MESSAGE_HISTORY));
  }, [flushAssistantStream]);

  const appendAssistantDelta = useCallback((value: string) => {
    if (!value) return;
    const stream = assistantStream.current ?? {
      id: nextMessageId(),
      raw: "",
      text: "",
      createdAt: Date.now(),
    };
    stream.raw += value;
    assistantStream.current = stream;
    if (assistantDeltaFrame.current === undefined) {
      assistantDeltaFrame.current = window.requestAnimationFrame(() => {
        assistantDeltaFrame.current = undefined;
        flushAssistantStream();
      });
    }
  }, [flushAssistantStream]);

  useEffect(() => () => {
    if (assistantDeltaFrame.current !== undefined) {
      window.cancelAnimationFrame(assistantDeltaFrame.current);
    }
  }, []);

  const copyMessage = useCallback(async (message: ConversationMessage) => {
    try {
      await copyTextToClipboard(message.text);
      setCopiedMessageId(message.id);
      window.setTimeout(() => {
        setCopiedMessageId((current) => current === message.id ? undefined : current);
      }, 1600);
    } catch (cause) {
      setError(toUserError(cause));
    }
  }, []);

  const refreshConfiguration = useCallback(async (refreshModels = false) => {
    const configuration = await loadSharedConfiguration();
    const catalog = await loadProviderCatalog(refreshModels, refreshModels ? configuration.provider : undefined);
    setSharedConfiguration(configuration);
    setProviderCatalog(catalog);
    setProvider(configuration.provider);
    const configuredProvider = catalog.find((entry) => entry.profileProvider === configuration.provider);
    setModel(configuredProvider?.browserOauth
      ? configuredProvider.modelOptions.includes(configuration.model)
        ? configuration.model
        : configuredProvider.modelOptions[0] ?? ""
      : configuration.model);
    setEffort(configuration.effort);
    setBaseUrl(configuration.baseUrl ?? catalog.find((entry) => entry.profileProvider === configuration.provider)?.baseUrl ?? "");
    if (
      configuration.credentialConfigured
      && catalog.find((entry) => entry.profileProvider === configuration.provider)?.browserOauth
    ) {
      setOauthAuthenticatedProvider(configuration.provider);
    }
    return configuration;
  }, []);

  const refreshWebArtifact = useCallback(async (id: string, failed = false, generation?: number) => {
    try {
      const artifact = await findWebArtifact(id);
      if (generation !== undefined && generation !== runtimeGeneration.current) return;
      if (artifact) {
        setWebArtifact(artifact);
        setPartialResult(failed);
        setSidePanelView("preview");
      }
    } catch {
      // Artifact discovery is a convenience after a turn; a discovery failure must not
      // replace the runtime's own result or error in the conversation.
    }
  }, []);

  const openArtifactExternally = useCallback(async () => {
    if (!runtimeId || !webArtifact) return;
    try {
      await openWebArtifact(runtimeId, webArtifact.path);
    } catch (cause) {
      setError(toUserError(cause));
    }
  }, [runtimeId, webArtifact]);

  const applyEvent = useCallback((event: RuntimeEvent) => {
    switch (event.type) {
      case "started":
        setBusy(true);
        setError(undefined);
        setPartialResult(false);
        turnStartedAt.current = Date.now();
        setTurnSummary(undefined);
        terminalEventHandled.current = false;
        assistantResponseInTurn.current = false;
        assistantStream.current = undefined;
        lastTransportError.current = undefined;
        break;
      case "assistantText":
        appendAssistantDelta(event.text);
        break;
      case "activity": {
        const activity = { ...event.activity, details: event.activity.details ?? [] };
        setActivities((current) => {
          if (!activity.id) return appendBounded(current, activity, MAX_ACTIVITY_ENTRIES);
          const index = current.findIndex((item) => item.id === activity.id);
          if (index < 0) return appendBounded(current, activity, MAX_ACTIVITY_ENTRIES);
          const next = [...current];
          next[index] = activity;
          return next;
        });
        const timestamp = Date.now();
        const activityId = activity.id;
        const newEntryId = nextWorkEntryId();
        const status = activity.kind === "done"
          ? "Done"
          : activity.kind === "error"
            ? "Error"
            : activity.kind === "tool" || activity.kind === "progress" || activity.kind === "verification"
              ? "Working"
              : "Recorded";
        setWorkLog((current) => {
          const index = activityId
            ? current.findIndex((item) => item.kind === "activity" && item.activityId === activityId)
            : -1;
          const entry: WorkLogEntry = {
            id: index >= 0 ? current[index].id : newEntryId,
            kind: "activity",
            activityId,
            text: activity.title,
            details: activity.details,
            status,
            timestamp,
          };
          if (index < 0) return appendBounded(current, entry, MAX_WORK_LOG_ENTRIES);
          const next = [...current];
          next[index] = entry;
          return next;
        });
        break;
      }
      case "plan":
        setPlan(event.steps);
        break;
      case "question":
        setQuestions(event.prompts);
        setBusy(false);
        break;
      case "usage":
        setUsage({
          input: event.inputTokens,
          output: event.outputTokens,
          cached: event.cacheReadInputTokens,
          cacheWrite: event.cacheCreationInputTokens,
          total: event.totalTokens,
          elapsed: event.durationMs,
        });
        break;
      case "progress":
        setTurn(event.turn);
        break;
      case "settings":
        setSettings({
          model: event.model,
          effort: event.effort,
          verbosity: parseVerbosity(event.verbosity),
          planMode: event.planMode,
          credentialConfigured: event.credentialConfigured,
        });
        break;
      case "configurationChanged":
        void refreshConfiguration().catch((cause) => setError(toUserError(cause)));
        break;
      case "notice":
        {
          const details = event.details ?? [];
          appendWorkLog({ kind: "status", text: event.title, status: "Info", details });
          if (event.title === "Completion report" && details.length && !assistantResponseInTurn.current) {
            appendAssistantMessage(details.join("\n\n"));
          }
        }
        break;
      case "newSession":
        setMessages([]);
        setActivities([]);
        setWorkLog([]);
        setPlan([]);
        setQuestions([]);
        setUsage(emptyUsage);
        setTurn(0);
        setLastRequest(undefined);
        setWebArtifact(undefined);
        setTurnSummary(undefined);
        turnStartedAt.current = undefined;
        setPartialResult(false);
        setSidePanelView("work");
        terminalEventHandled.current = false;
        assistantResponseInTurn.current = false;
        assistantStream.current = undefined;
        lastTransportError.current = undefined;
        setBusy(false);
        break;
      case "compacted":
        appendWorkLog({ kind: "status", text: event.message, status: "Context updated" });
        break;
      case "completed":
        if (terminalEventHandled.current) break;
        terminalEventHandled.current = true;
        flushAssistantStream();
        setBusy(false);
        setActivities((current) => finishActivities(current, "done", "Turn completed."));
        appendWorkLog({ kind: "status", text: "Final response ready", status: "Done" });
        setTurnSummary({ status: "completed", elapsedSeconds: Math.max(0, Math.round((Date.now() - (turnStartedAt.current ?? Date.now())) / 1000)) });
        turnStartedAt.current = undefined;
        if (!assistantResponseInTurn.current) {
          appendAssistantMessage("The request completed successfully, but Medusa did not return a chat summary. Check Work for the execution details.");
        }
        break;
      case "turnFinished":
        if (terminalEventHandled.current) break;
        terminalEventHandled.current = true;
        flushAssistantStream();
        setBusy(false);
        setActivities((current) => finishActivities(current, "done", "Turn finished."));
        appendWorkLog({ kind: "status", text: "Turn finished", status: "Done" });
        setTurnSummary({ status: "completed", elapsedSeconds: Math.max(0, Math.round((Date.now() - (turnStartedAt.current ?? Date.now())) / 1000)) });
        turnStartedAt.current = undefined;
        if (!assistantResponseInTurn.current) {
          appendAssistantMessage("The turn finished successfully, but Medusa did not return a chat summary. Check Work for the execution details.");
        }
        break;
      case "cancelled":
        setBusy(false);
        setActivities((current) => finishActivities(current, "error", "Stopped because the turn was cancelled."));
        appendWorkLog({ kind: "status", text: "Turn stopped", status: "Stopped" });
        setTurnSummary({ status: "cancelled", elapsedSeconds: Math.max(0, Math.round((Date.now() - (turnStartedAt.current ?? Date.now())) / 1000)) });
        turnStartedAt.current = undefined;
        appendAssistantMessage("The turn was stopped before completion. You can retry the last request when you are ready.");
        break;
      case "failed":
        setBusy(false);
        setActivities((current) => finishActivities(current, "error", `Stopped because the runtime failed: ${event.message}`));
        setError(event.message);
        setPartialResult(false);
        appendWorkLog({ kind: "status", text: "Turn failed", status: "Error", details: [event.message] });
        setTurnSummary({
          status: "failed",
          elapsedSeconds: Math.max(0, Math.round((Date.now() - (turnStartedAt.current ?? Date.now())) / 1000)),
          error: event.message,
        });
        turnStartedAt.current = undefined;
        appendAssistantMessage(`The request did not complete because the runtime reported an error:\n\n${event.message}\n\nRetry the request or inspect Work for the failed execution step. If Preview is available, it contains the partial result that was produced before the failure.`);
        break;
    }
  }, [appendAssistantDelta, appendAssistantMessage, appendWorkLog, flushAssistantStream, refreshConfiguration]);

  useEffect(() => {
    if (!busy) {
      setLiveElapsedSeconds(0);
      return;
    }
    const updateElapsed = () => {
      const startedAt = turnStartedAt.current;
      if (startedAt === undefined) return;
      setLiveElapsedSeconds(Math.max(0, Math.floor((Date.now() - startedAt) / 1000)));
    };
    updateElapsed();
    const interval = window.setInterval(updateElapsed, 1000);
    return () => window.clearInterval(interval);
  }, [busy]);

  useEffect(() => {
    const transcript = transcriptRef.current;
    if (!transcript || typeof transcript.scrollTo !== "function") return;
    const distanceFromBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight;
    if (distanceFromBottom > 120) return;
    transcript.scrollTo({ top: transcript.scrollHeight, behavior: busy ? "auto" : "smooth" });
  }, [messages, activities, busy]);

  useEffect(() => {
    if (!runtimeId) return;
    let active = true;
    const generation = runtimeGeneration.current;
    const pollingRuntimeId = runtimeId;
    let timer: number | undefined;
    const schedule = (delay: number) => {
      if (!active) return;
      if (timer !== undefined) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = undefined;
        void poll();
      }, delay);
    };
    const poll = async () => {
      if (!active) return;
      if (pollBusy.current) {
        schedule(20);
        return;
      }
      pollBusy.current = true;
      try {
        const events = await pollRuntime(runtimeId);
        if (!active || generation !== runtimeGeneration.current || pollingRuntimeId !== runtimeId) return;
        transportFailureCount.current = 0;
        if (transportErrorVisible.current) {
          transportErrorVisible.current = false;
          lastTransportError.current = undefined;
        }
        setError((current) => current?.startsWith("dependency unavailable: daemon") || current?.startsWith("daemon transport error:") ? undefined : current);
        events.forEach(applyEvent);
        const terminalEvent = events.find((event) => event.type === "completed" || event.type === "turnFinished" || event.type === "failed" || event.type === "cancelled");
        if (terminalEvent) {
          void refreshWebArtifact(runtimeId, terminalEvent.type === "failed", generation);
        }
      } catch (cause) {
        if (active) {
          transportFailureCount.current += 1;
          // A single local IPC reset is recoverable: the daemon request is retried
          // below and the next poll normally resumes from the durable cursor. Keep
          // transient socket noise out of the transcript unless it persists.
          if (transportFailureCount.current < 3) return;
          const message = toUserError(cause);
          setError(message);
          transportErrorVisible.current = true;
          if (lastTransportError.current !== message) {
            lastTransportError.current = message;
            appendAssistantMessage(`The runtime could not finish the request because communication with Medusa failed:\n\n${message}`);
          }
        }
      } finally {
        pollBusy.current = false;
        schedule(busyRef.current ? 80 : 750);
      }
    };
    const wake = () => {
      if (!active) return;
      if (timer !== undefined) {
        window.clearTimeout(timer);
        timer = undefined;
      }
      void poll();
    };
    wakePoll.current = wake;
    schedule(80);
    return () => {
      active = false;
      if (wakePoll.current === wake) wakePoll.current = undefined;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [runtimeId, applyEvent, appendAssistantMessage, refreshWebArtifact]);

  useEffect(() => {
    if (busy) wakePoll.current?.();
  }, [busy]);

  useEffect(() => {
    if (!runtimeId || !prompt.trimStart().startsWith("/") || prompt.includes("\n")) {
      setSlashSuggestions([]);
      return;
    }
    let active = true;
    const timer = window.setTimeout(() => {
      void commandSuggestions(runtimeId, prompt)
        .then((suggestions) => {
          if (!active) return;
          setSlashSuggestions(suggestions);
          setSlashSelection(0);
        })
        .catch((cause) => {
          if (active) setError(toUserError(cause));
        });
    }, 75);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [runtimeId, prompt]);

  useEffect(() => {
    const option = slashOptionRefs.current[slashSelection];
    if (option && typeof option.scrollIntoView === "function") {
      option.scrollIntoView({ block: "nearest" });
    }
  }, [slashSelection, slashSuggestions.length]);

  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    let disposed = false;
    const start = async () => {
      runtimeGeneration.current += 1;
      const configuration = await refreshConfiguration();
      if (!configuration.configured || !configuration.provider.trim() || !configuration.model.trim()) return undefined;
      if (configuration.provider === "openai-oauth") {
        // OAuth discovery talks to the Codex app-server and can take its full
        // protocol timeout. Warm it in the background so launch stays usable.
        void ensureBrowserOauth(configuration.provider)
          .then(() => {
            if (!disposed) void refreshConfiguration(true).catch(() => undefined);
          })
          .catch(() => undefined);
      }
      let started;
      try {
        started = await startRuntime(previous || undefined);
      } catch (cause) {
        if (!previous) throw cause;
        publishRepoChanged("");
        started = await startRuntime();
      }
      if (disposed) {
        void closeRuntime(started.runtimeId);
        return undefined;
      }
      // Publish the runtime before provider verification/configuration. The
      // daemon may need to start or recover, but that must not make the
      // composer unavailable or make the whole window appear hung.
      setRuntimeId(started.runtimeId);
      setActiveRepo(started.repo);
      try {
        return await configureStartedRuntime(started, {
          provider: configuration.provider,
          model: configuration.model,
          effort: configuration.effort,
          expectedRevision: configuration.revision,
        }, { preserveRuntimeOnDependencyFailure: true });
      } catch (cause) {
        if (!disposed && !String(cause).includes("dependency unavailable: daemon")) {
          setRuntimeId(undefined);
          setActiveRepo("");
        }
        throw cause;
      }
    };
    void start()
      .then((started) => {
        if (!started) return;
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
      })
      .catch((cause) => {
        if (!disposed) setError(toUserError(cause));
      });
    return () => {
      disposed = true;
    };
  }, [refreshConfiguration]);

  useEffect(() => () => {
    if (runtimeId) void closeRuntime(runtimeId);
  }, [runtimeId]);

  const resumeSession = useCallback(async (sessionId: string) => {
    if (resumeInFlight.current || runtimeTransitionInFlight.current) return;
    resumeInFlight.current = true;
    runtimeTransitionInFlight.current = true;
    setError(undefined);
    let started: Awaited<ReturnType<typeof resumeRuntime>> | undefined;
    try {
      started = await resumeRuntime(activeRepoRef.current, sessionId);
      await configureStartedRuntime(started, {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
      });
      // Keep the existing subscription valid until the replacement runtime has
      // passed configuration. A failed transition must leave the current runtime
      // polling with its original generation.
      runtimeGeneration.current += 1;
      setRuntimeId(started.runtimeId);
      setActiveRepo(started.repo);
      setMessages([]);
      setActivities([]);
      setWorkLog([]);
      setPlan([]);
      setQuestions([]);
      setLastRequest(undefined);
      setWebArtifact(undefined);
      setTurnSummary(undefined);
      turnStartedAt.current = undefined;
      setPartialResult(false);
      setSidePanelView("work");
    } catch (cause) {
      // resumeRuntime activates the candidate before configuration can be
      // verified. Restore the original runtime's subscribers after rejecting it.
      if (started && runtimeId && started.runtimeId !== runtimeId) restoreRuntime(runtimeId);
      setError(toUserError(cause));
    } finally {
      resumeInFlight.current = false;
      runtimeTransitionInFlight.current = false;
    }
  }, [effort, model, provider, setActiveRepo, sharedConfiguration?.revision]);

  useEffect(() => {
    const onResumeRequest = (event: Event) => {
      const detail = (event as CustomEvent<{ sessionId?: string; repo?: string } | string>).detail;
      const sessionId = typeof detail === "string" ? detail : detail?.sessionId;
      const requestedRepo = typeof detail === "string" ? "" : detail?.repo?.trim() ?? "";
      if (requestedRepo && requestedRepo !== activeRepoRef.current) {
        setError("That saved session belongs to a different project. Open that project before resuming it.");
        return;
      }
      if (sessionId) void resumeSession(sessionId);
    };
    window.addEventListener(RUNTIME_RESUME_EVENT, onResumeRequest);
    return () => window.removeEventListener(RUNTIME_RESUME_EVENT, onResumeRequest);
  }, [resumeSession]);

  const openProject = async () => {
    if (runtimeTransitionInFlight.current) return;
    const selected = await open({ directory: true, multiple: false, title: "Open a Medusa project" });
    if (typeof selected !== "string") return;
    runtimeTransitionInFlight.current = true;
    let started: Awaited<ReturnType<typeof startRuntime>> | undefined;
    try {
      started = await startRuntime(selected);
      // Configure the candidate before changing the active runtime. This keeps
      // the current project and its polling subscription usable if startup or
      // provider verification fails.
      await configureStartedRuntime(started, {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
      });
      runtimeGeneration.current += 1;
      setRuntimeId(started.runtimeId);
      setActiveRepo(started.repo);
      setMessages([]);
      setActivities([]);
      setWorkLog([]);
      setPlan([]);
      setQuestions([]);
      setLastRequest(undefined);
      setWebArtifact(undefined);
      setTurnSummary(undefined);
      turnStartedAt.current = undefined;
      setPartialResult(false);
      setSidePanelView("work");
      setError(undefined);
      publishRepoChanged(started.repo);
      await refreshConfiguration();
    } catch (cause) {
      if (started && runtimeId && started.runtimeId !== runtimeId) restoreRuntime(runtimeId);
      setError(toUserError(cause));
    } finally {
      runtimeTransitionInFlight.current = false;
    }
  };

  const openGeneralChat = async () => {
    if (runtimeTransitionInFlight.current) return;
    runtimeTransitionInFlight.current = true;
    let started: Awaited<ReturnType<typeof startRuntime>> | undefined;
    try {
      started = await startRuntime();
      await configureStartedRuntime(started, {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
      });
      runtimeGeneration.current += 1;
      setRuntimeId(started.runtimeId);
      setActiveRepo("");
      setMessages([]);
      setActivities([]);
      setWorkLog([]);
      setPlan([]);
      setQuestions([]);
      setLastRequest(undefined);
      setWebArtifact(undefined);
      setTurnSummary(undefined);
      turnStartedAt.current = undefined;
      setPartialResult(false);
      setSidePanelView("work");
      setError(undefined);
      publishRepoChanged("");
      await refreshConfiguration();
    } catch (cause) {
      if (started && runtimeId && started.runtimeId !== runtimeId) restoreRuntime(runtimeId);
      setError(toUserError(cause));
    } finally {
      runtimeTransitionInFlight.current = false;
    }
  };

  const addFiles = async () => {
    if (!repo) return;
    const selected = await open({ multiple: true, directory: false, title: "Attach repository files" });
    const paths = typeof selected === "string" ? [selected] : selected ?? [];
    if (!paths.length) return;
    markComposerEdited();
    setAttachments((current) => [
      ...current,
      ...paths.map((path): DesktopAttachment => ({ kind: "file", path })),
    ]);
  };

  const addImages = async (files: File[]) => {
    if (!files.length) return;
    try {
      const next = await Promise.all(files.map(readImage));
      markComposerEdited();
      setAttachments((current) => [...current, ...next]);
      setError(undefined);
    } catch (cause) {
      setError(toUserError(cause));
    }
  };

  const imageCompatibility = useMemo(() => {
    const state = profileModelCapabilityState(
      providerCatalog,
      provider,
      sharedConfiguration?.connection,
      model,
      "image_input",
    );
    if (state === "supported") {
      return { supported: true, text: `${model} is configured for image input.` };
    }
    if (state === "unsupported") {
      return { supported: false, text: `${model} does not advertise image input on this route.` };
    }
    return { supported: undefined, text: "Image compatibility will be verified by the runtime before upload." };
  }, [providerCatalog, provider, model, sharedConfiguration?.connection]);

  const configureSelectedModelForTurn = async () => {
    if (!runtimeId || !provider.trim() || !model.trim()) return;
    const selected = providerCatalog.find((entry) => entry.profileProvider === provider);
    const selectedProviderIsReady = selected?.credentialConfigured === true
      || selected?.profileProvider === sharedConfiguration?.provider;
    if (selected && !selectedProviderIsReady) {
      throw new Error(`${selected.displayName} is not configured in Settings yet.`);
    }
    const alreadyActive = sharedConfiguration?.provider === provider
      && sharedConfiguration.model === model
      && sharedConfiguration.effort === effort;
    if (alreadyActive) return;

    await configureRuntime(runtimeId, {
      provider,
      model,
      effort,
      expectedRevision: sharedConfiguration?.revision ?? 0,
      baseUrl: selected?.customValues ? baseUrl.trim() || undefined : undefined,
    });
    const configuration = await loadSharedConfiguration();
    setSharedConfiguration(configuration);
    if (configuration.credentialConfigured && configuration.provider === "openai-oauth") {
      setOauthAuthenticatedProvider(configuration.provider);
    }
  };

  const onPaste = async (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const images = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"));
    if (!images.length) return;
    event.preventDefault();
    await addImages(images);
  };

  const sendText = async (
    text: string,
    suppliedAttachments = attachments,
    existingMessageId?: number,
  ) => {
    if (!runtimeId || (!text.trim() && suppliedAttachments.length === 0)) return;
    if (suppliedAttachments.some((attachment) => attachment.kind === "image") && imageCompatibility.supported === false) {
      setError(`${imageCompatibility.text} Switch model/route or remove the image.`);
      return;
    }
    const clean = text.trim();
    const submitsTurn = !(clean.startsWith("/") && suppliedAttachments.length === 0);
    const submittedComposer = composerSnapshot();
    if (submitsTurn && submitInFlight.current) return;
    if (submitsTurn) submitInFlight.current = true;
    setError(undefined);
    setQuestions([]);
    // A new user turn supersedes any persisted stop notice from an older run.
    // The runtime still owns durable recovery state; this only keeps stale
    // inline guidance from appearing alongside a live execution.
    if (submitsTurn) dismissRecovery();
    // Mark the turn as active before the IPC round-trip. Creating a session can include
    // capability probes and daemon startup, so waiting for `runtime_submit` to resolve
    // left the UI looking idle precisely while the background work was already running.
    if (submitsTurn) {
      setBusy(true);
      setPendingSubmit(true);
    }
    if (submitsTurn) {
      assistantResponseInTurn.current = false;
      terminalEventHandled.current = false;
      lastTransportError.current = undefined;
      setWebArtifact(undefined);
      setTurnSummary(undefined);
      turnStartedAt.current = Date.now();
      setPartialResult(false);
      setSidePanelView("work");
    }
    appendWorkLog({
      kind: "input",
      text: clean || suppliedAttachments.map((attachment) => attachment.kind === "file" ? basename(attachment.path) : attachment.name).join(", ") || "Attached context",
      status: "Sent",
    });
    const userMessageId = existingMessageId ?? nextMessageId();
    setMessages((current) => existingMessageId
      ? current.map((message) => message.id === existingMessageId ? { ...message, failed: false, queued: false } : message)
      : appendBounded(current, {
          id: userMessageId,
          role: "user",
          text: text || "Attached context",
          createdAt: Date.now(),
          attachments: suppliedAttachments,
        }, MAX_MESSAGE_HISTORY));
    if (submitsTurn) {
      setLastRequest({ text, attachments: suppliedAttachments });
      // Keep the durable draft while transport is pending; a failed send restores it below.
      // Clearing the visible composer immediately lets the user prepare the next turn.
      if (isCurrentComposerSnapshot(submittedComposer) && isActiveComposerScope(submittedComposer)) {
        setPrompt("");
        setAttachments([]);
      }
    }
    try {
      // Keep the exact submitted snapshot recoverable if the user changes
      // projects while the asynchronous transport is in flight.
      persistDesktopDraft(submittedComposer.repo, text, suppliedAttachments);
    } catch {
      // Draft persistence is best effort and must never reject a send.
    }
    try {
      if (clean.startsWith("/") && suppliedAttachments.length === 0) {
        await runRuntimeCommand(runtimeId, clean);
        clearAcceptedComposer(submittedComposer);
      } else {
        await configureSelectedModelForTurn();
        const disposition = await submitRuntime(runtimeId, {
          text,
          attachments: suppliedAttachments,
          revision: Date.now(),
        });
        setMessages((current) => current.map((message) => message.id === userMessageId
            ? { ...message, queued: disposition === "queued", failed: false }
            : message));
        failedMessage.current = undefined;
        clearAcceptedComposer(submittedComposer);
      }
    } catch (cause) {
      if (submitsTurn) {
        setBusy(false);
        setPendingSubmit(false);
      }
      const message = toUserError(cause);
      setError(message);
      if (submitsTurn) {
        failedMessage.current = { id: userMessageId, text, attachments: suppliedAttachments };
        setMessages((current) => current.map((entry) => entry.id === userMessageId ? { ...entry, failed: true, queued: false } : entry));
        // Keep a rejected request editable. Do not overwrite new text entered
        // while the transport was in flight.
        if (isCurrentComposerSnapshot(submittedComposer) && isActiveComposerScope(submittedComposer)) {
          setPrompt((current) => current || text);
          setAttachments((current) => current.length ? current : suppliedAttachments);
        }
        appendAssistantMessage(`Medusa could not start the request:\n\n${message}`);
      }
    } finally {
      if (submitsTurn) {
        submitInFlight.current = false;
        setPendingSubmit(false);
      }
    }
  };

  const submit = async () => sendText(prompt);

  const retryLastRequest = async () => {
    setError(undefined);
    if (!lastRequest) {
      composerRef.current?.focus();
      return;
    }
    await sendText(lastRequest.text, lastRequest.attachments, failedMessage.current?.id);
  };

  const selectSlashSuggestion = (suggestion: CommandSuggestion) => {
    markComposerEdited();
    setPrompt(`/${suggestion.name} `);
    setSlashSuggestions([]);
    setSlashSelection(0);
  };

  const completeOnboarding = async (next: { provider: string; model: string; effort: Effort; apiKey?: string; baseUrl?: string }) => {
    const started = await startRuntime();
    try {
      await configureRuntime(started.runtimeId, {
        provider: next.provider,
        model: next.model,
        effort: next.effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
        apiKey: next.apiKey,
        baseUrl: next.baseUrl,
      });
      const configuration = await refreshConfiguration();
      setProvider(configuration.provider);
      setModel(configuration.model);
      setEffort(configuration.effort);
      setRuntimeId(started.runtimeId);
      setActiveRepo(started.repo);
      setError(undefined);
    } catch (cause) {
      await closeRuntime(started.runtimeId).catch(() => undefined);
      setError(toUserError(cause));
      throw cause;
    }
  };

  const selectProvider = async (value: string) => {
    const requestId = ++providerRequestId.current;
    authenticationRequestId.current += 1;
    const nextProvider = providerCatalog.find((entry) => entry.profileProvider === value);
    setProvider(value);
    setApiKey("");
    setError(undefined);
    setBaseUrl(nextProvider?.baseUrl ?? "");
    setModel(nextProvider?.browserOauth ? "" : nextProvider?.defaultModel ?? "");
    if (!nextProvider) return;

    setLoadingModels(true);
    try {
      if (nextProvider.browserOauth) {
        await ensureBrowserOauth(value);
      }
      const refreshed = await loadProviderCatalog(true, value);
      if (requestId !== providerRequestId.current) return;
      setProviderCatalog(refreshed);
      const refreshedProvider = refreshed.find((entry) => entry.profileProvider === value);
      if (refreshedProvider) {
        setModel(refreshedProvider.modelOptions[0] ?? (refreshedProvider.browserOauth ? "" : refreshedProvider.defaultModel));
      }
    } catch (cause) {
      if (requestId === providerRequestId.current) setError(toUserError(cause));
    } finally {
      if (requestId === providerRequestId.current) setLoadingModels(false);
    }
  };

  const authenticateSelectedProvider = async () => {
    if (!provider) return;
    const providerAtStart = provider;
    const requestId = ++authenticationRequestId.current;
    setAuthenticating(true);
    setError(undefined);
    try {
      await startBrowserOauth(providerAtStart);
      const refreshed = await loadProviderCatalog(true, providerAtStart);
      if (requestId !== authenticationRequestId.current || provider !== providerAtStart) return;
      setOauthAuthenticatedProvider(providerAtStart);
      setProviderCatalog(refreshed);
      const refreshedProvider = refreshed.find((entry) => entry.profileProvider === provider);
      if (refreshedProvider) {
        setModel((current) => refreshedProvider.modelOptions.includes(current)
          ? current
          : refreshedProvider.modelOptions[0] ?? (refreshedProvider.browserOauth ? "" : refreshedProvider.defaultModel));
      }
    } catch (cause) {
      if (requestId === authenticationRequestId.current && provider === providerAtStart) {
        setError(toUserError(cause));
      }
    } finally {
      if (requestId === authenticationRequestId.current) setAuthenticating(false);
    }
  };

  const applyModel = async () => {
    if (!runtimeId) return;
    try {
      await configureRuntime(runtimeId, {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
        apiKey: apiKey.trim() || undefined,
        baseUrl: providerCatalog.find((entry) => entry.profileProvider === provider)?.customValues
          ? baseUrl.trim() || undefined
          : undefined,
      });
      await refreshConfiguration();
      setApiKey("");
      setError(undefined);
    } catch (cause) {
      setError(toUserError(cause));
    }
  };

  const cancel = async () => {
    if (!runtimeId) return;
    // Release the composer immediately so the user can decide what to do next;
    // the runtime cancellation remains authoritative and is still awaited below.
    setBusy(false);
    setPendingSubmit(false);
    try {
      await cancelRuntime(runtimeId);
    } catch (cause) {
      setBusy(true);
      setError(toUserError(cause));
    }
  };

  const newSession = useCallback(async () => {
    if (!runtimeId) return;
    setActivePanel("chat");
    try {
      await runRuntimeCommand(runtimeId, "/new");
    } catch (cause) {
      setError(toUserError(cause));
    }
  }, [runtimeId]);

  const macPlatform = isMacPlatform();
  const newSessionShortcut = macPlatform ? "⌘N" : "Ctrl+N";

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const primaryModifier = macPlatform
        ? event.metaKey && !event.ctrlKey
        : event.ctrlKey && !event.metaKey;
      if (!primaryModifier || event.altKey || event.shiftKey || event.key.toLowerCase() !== "n") return;
      event.preventDefault();
      void newSession();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [macPlatform, newSession]);

  const selectedProvider = useMemo(
    () => providerCatalog.find((entry) => entry.profileProvider === provider),
    [providerCatalog, provider],
  );
  const effortOptions = useMemo(
    () => effortOptionsForModel(selectedProvider, model),
    [selectedProvider, model],
  );
  useEffect(() => {
    if (!effortOptions.includes(effort)) setEffort(effortOptions[0]);
  }, [effort, effortOptions]);
  const oauthProvider = selectedProvider?.browserOauth ?? false;
  const oauthAuthenticated = oauthProvider && (
    oauthAuthenticatedProvider === provider
    || selectedProvider?.credentialConfigured === true
    || (sharedConfiguration?.provider === provider && sharedConfiguration.credentialConfigured)
  );
  const credentiallessProvider = !oauthProvider
    && (selectedProvider?.authMethods.every((method) => method === "none") ?? false);
  const composerProviders = useMemo(
    () => providerCatalog.filter((entry) =>
      !entry.disabledReason
      && (entry.profileProvider === provider || entry.credentialConfigured === true),
    ),
    [providerCatalog, provider],
  );
  const selectedModelDisplayName = selectedProvider?.models?.find((candidate) => candidate.id === model)?.display_name;
  const composerSelectorLabel = selectedProvider && model
    ? selectedModelDisplayName ?? model
    : "Choose model";
  const repoName = useMemo(() => basename(repo) || "General chat", [repo]);
  const totalTokens = usage.total;
  const openDesktopTool = (tool: DesktopTool) => requestDesktopTool(tool);
  let activeWorkEntry: WorkLogEntry | undefined;
  for (let index = workLog.length - 1; index >= 0; index -= 1) {
    const entry = workLog[index];
    if (entry?.kind === "activity" && entry.status === "Working") {
      activeWorkEntry = entry;
      break;
    }
  }
  // /verbose display filter: tool-progress rows carry status "Working".
  // "off" hides them, "new" keeps only the latest, "verbose" expands details.
  const visibleWorkLog = (() => {
    if (settings.verbosity === "all" || settings.verbosity === "verbose") return workLog;
    let latestWorking = -1;
    if (settings.verbosity === "new") {
      for (let index = workLog.length - 1; index >= 0; index -= 1) {
        const entry = workLog[index];
        if (entry?.kind === "activity" && entry.status === "Working") {
          latestWorking = index;
          break;
        }
      }
    }
    return workLog.filter((entry, index) =>
      entry.kind !== "activity" || entry.status !== "Working" || index === latestWorking,
    );
  })();
  const verboseDetails = settings.verbosity === "verbose";
  const liveActivityEntries = visibleWorkLog
    .filter((entry) => entry.kind === "activity")
    .slice(-8);
  const completedActivityEntries = workLog.filter((entry) => entry.kind === "activity");
  const hasPartialResult = partialResult && Boolean(webArtifact);

  const beginSidePanelResize = (event: React.PointerEvent<HTMLButtonElement>) => {
    event.preventDefault();
    sidePanelResizeStart.current = { x: event.clientX, width: sidePanelWidth };
    setSidePanelResizing(true);
  };

  useEffect(() => {
    if (!sidePanelResizing) return;
    const onMove = (event: PointerEvent) => {
      const start = sidePanelResizeStart.current;
      if (!start) return;
      const maxWidth = Math.max(420, Math.min(900, window.innerWidth * 0.72));
      setSidePanelWidth(Math.max(280, Math.min(maxWidth, start.width + start.x - event.clientX)));
    };
    const onUp = () => {
      sidePanelResizeStart.current = undefined;
      setSidePanelResizing(false);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [sidePanelResizing]);

  if (!runtimeId && sharedConfiguration && !sharedConfiguration.configured) {
    return (
      <DesktopOnboarding
        configuration={sharedConfiguration}
        providers={providerCatalog}
        error={error}
        onApply={completeOnboarding}
      />
    );
  }

  return (
    <>
    <main
      className={`app-shell medusa-shell${railCollapsed ? " rail-collapsed" : ""}${sidePanelView ? "" : " work-panel-collapsed"}`}
      style={{ "--medusa-side-panel-width": `${sidePanelWidth}px` } as React.CSSProperties}
    >
      <aside className="sidebar" aria-label="Session rail">
        <div className="window-dots" aria-hidden="true">
          <span className="dot red" /><span className="dot yellow" /><span className="dot green" />
        </div>
        <div className="brand-row">
          <span className="brand-mark"><Bot size={17} /></span>
          <div className="rail-label"><h1>Medusa</h1><small>Desktop</small></div>
          <span className="version rail-label">v{packageMetadata.version}</span>
        </div>
        <button className="new-session" onClick={newSession} disabled={!runtimeId} aria-keyshortcuts={macPlatform ? "Meta+N" : "Control+N"} title="New Chat">
          <span><Plus size={16} /><span className="rail-label">New Chat</span></span><kbd className="rail-label">{newSessionShortcut}</kbd>
        </button>
        <nav className="nav-list" aria-label="Workspace views">
          <button className={`nav-item ${activePanel === "settings" ? "active" : ""}`} onClick={() => setActivePanel("settings")} title="Settings">
            <Settings size={17} /><span className="rail-label">Settings</span>
          </button>
        </nav>
        <section className="project-card">
          <p className="section-label rail-label">Context</p>
          <button className="project-picker" onClick={openProject} title={`Open project: ${repoName}`}>
            <FolderOpen size={17} />
            <span className="rail-label"><strong>{repoName}</strong><small>{repo || "No project attached"}</small></span>
            <ChevronRight className="rail-label" size={15} />
          </button>
          {!!repo && <button className="projectless-action rail-label" onClick={openGeneralChat}>Switch to general chat</button>}
        </section>
        <section className="rail-tools">
          <SessionDock />
          <button className="nav-item" onClick={() => openDesktopTool("learning")} title="Learning"><GraduationCap size={17} /><span className="rail-label">Learning</span></button>
          <button className="nav-item" onClick={() => openDesktopTool("engineering")} title="Engineering"><BarChart3 size={17} /><span className="rail-label">Engineering</span></button>
        </section>
        <div className="sidebar-spacer" />
        <div className="security-note"><ShieldCheck size={15} /><span className="rail-label">Medusa policy remains authoritative</span></div>
      </aside>

      <section className="workspace medusa-workspace">
        <header className="topbar">
          <div className="topbar-title">
            <button className="rail-toggle" onClick={() => setRailCollapsed((current) => !current)} aria-label={railCollapsed ? "Expand session rail" : "Collapse session rail"} aria-expanded={!railCollapsed}>
              {railCollapsed ? <PanelLeftOpen size={18} /> : <PanelLeftClose size={18} />}
            </button>
            <div>
              <p className="eyebrow">{activePanel === "chat" ? "Interactive session" : activePanel}</p>
              <h2>{repoName}</h2>
            </div>
          </div>
          <div className="topbar-actions">
            {webArtifact && (
              <button
                className="details-button"
                onClick={() => setSidePanelView((current) => current === "preview" ? undefined : "preview")}
                aria-expanded={sidePanelView === "preview"}
                aria-controls="side-panel"
              >
                {sidePanelView === "preview" ? <PanelRightClose size={15} /> : <PanelRightOpen size={15} />} Rendered webpage
              </button>
            )}
            <button className="details-button" onClick={() => setSidePanelView((current) => current === "details" ? undefined : "details")} aria-expanded={sidePanelView === "details"} aria-controls="side-panel">
              <Info size={16} /> Session details
            </button>
            <button className="details-button work-panel-toggle" onClick={() => setSidePanelView((current) => current === "work" ? undefined : "work")} aria-expanded={sidePanelView === "work"} aria-controls="side-panel">
              {sidePanelView === "work" ? <PanelRightClose size={16} /> : <PanelRightOpen size={16} />} Work{workLog.length ? ` · ${workLog.length}` : ""}
            </button>
            <div className="runtime-state" role="status">
              <span className={`status-dot ${busy ? "busy" : hasPartialResult ? "ready" : error ? "offline" : runtimeId ? "ready" : "offline"}`} />
              {busy ? `Working · turn ${turn}` : hasPartialResult ? "Result available" : error ? "Needs attention" : runtimeId ? "Ready" : "Starting"}
            </div>
          </div>
        </header>

        {sidePanelView && sidePanelHost && createPortal((
          <aside
            id="side-panel"
            className={`work-panel side-panel ${sidePanelResizing ? "resizing" : ""}`}
            role="complementary"
            aria-label={sidePanelView === "preview" ? "Rendered webpage" : sidePanelView === "details" ? "Session details" : "Work"}
          >
            <button
              className="side-panel-resize-handle"
              type="button"
              aria-label="Resize side panel"
              aria-keyshortcuts="ArrowLeft ArrowRight"
              title="Drag to resize"
              onPointerDown={beginSidePanelResize}
              onKeyDown={(event) => {
                if (event.key === "ArrowLeft") setSidePanelWidth((current) => Math.min(900, current + 24));
                if (event.key === "ArrowRight") setSidePanelWidth((current) => Math.max(280, current - 24));
              }}
            />

            {sidePanelView === "details" ? (
              <>
                <div className="work-panel-heading"><div><p className="eyebrow">Progressive disclosure</p><h2>Session details</h2></div><button className="icon-button" onClick={() => setSidePanelView(undefined)} aria-label="Close session details"><X size={17} /></button></div>
                <div className="side-panel-scroll">
                  <section className="details-section">
                    <div className="panel-heading"><span><Gauge size={15} /> Runtime</span></div>
                    <dl className="metric-grid">
                      <div><dt>Model</dt><dd>{settings.model}</dd></div>
                      <div><dt>Effort</dt><dd>{settings.effort.replace("effort:", "")}</dd></div>
                      <div><dt>Mode</dt><dd>{settings.planMode ? "Plan" : "Full"}</dd></div>
                      <div><dt>Credential</dt><dd>{settings.credentialConfigured || oauthAuthenticatedProvider === provider ? "Ready" : "Missing"}</dd></div>
                    </dl>
                  </section>
                  <section className="details-section">
                    <div className="panel-heading"><span><Activity size={15} /> Usage</span></div>
                    <dl className="metric-grid tokens">
                      <div><dt>Input</dt><dd>{usage.input.toLocaleString()}</dd></div>
                      <div><dt>Output</dt><dd>{usage.output.toLocaleString()}</dd></div>
                      <div><dt>Cached</dt><dd>{usage.cached.toLocaleString()}</dd></div>
                      <div><dt>Total</dt><dd>{totalTokens.toLocaleString()}</dd></div>
                    </dl>
                    <p className="metric-footnote">Model time: {(usage.elapsed / 1000).toFixed(1)}s</p>
                  </section>
                  <section className="details-section">
                    <div className="panel-heading"><span><ListChecks size={15} /> Plan</span><small>{plan.filter((step) => step.status === "completed").length}/{plan.length}</small></div>
                    <div className="mini-plan">{plan.length ? plan.map((step) => <div key={step.title} className={step.status}>{planIcon(step.status)}<span>{step.title}</span></div>) : <p>No active plan</p>}</div>
                  </section>
                </div>
              </>
            ) : sidePanelView === "preview" && webArtifact ? (
              <div className="side-panel-preview">
                <div className="work-panel-heading"><div><p className="eyebrow">Live preview</p><h2>Rendered webpage</h2></div><button className="icon-button" onClick={() => setSidePanelView(undefined)} aria-label="Collapse rendered webpage panel"><X size={17} /></button></div>
                <p className="web-artifact-title">{webArtifact.title}</p>
                <p className="web-artifact-path" title={webArtifact.path}>{basename(webArtifact.path)}</p>
                <p className="web-artifact-copy">This page is rendered directly in Medusa. Drag the panel edge to resize it, or switch views from the top bar.</p>
                {hasPartialResult && <p className="web-artifact-status" role="status">The rendered result is available, but one execution step reported an error. Inspect Work for technical details.</p>}
                <div className="web-artifact-preview" aria-label="Rendered webpage preview">
                  <iframe
                    key={webArtifact.path}
                    title={webArtifact.title}
                    src={webArtifactPreviewUrl(webArtifact.path)}
                    sandbox="allow-forms allow-modals allow-popups allow-presentation allow-scripts"
                  />
                </div>
              </div>
            ) : (
              <>
                <div className="work-panel-heading">
                  <div>
                    <p className="eyebrow">Activity log</p>
                    <h2>Work</h2>
                  </div>
                  <button className="icon-button" onClick={() => setSidePanelView(undefined)} aria-label="Collapse work panel" title="Collapse work panel">
                    <PanelRightClose size={17} />
                  </button>
                </div>
                <div className="work-log" aria-label="Activity log">
                  {workLog.length === 0 ? (
                    <p className="work-log-empty">Actions and your inputs will appear here while Medusa works.</p>
                  ) : visibleWorkLog.map((entry) => (
                    entry.kind === "activity" ? (
                      <details className={`work-log-row activity ${entry.status === "Done" ? "done" : entry.status === "Error" ? "error" : ""}`} key={entry.id} open={verboseDetails || undefined}>
                        <summary>
                          <span className="work-log-icon">{entry.status === "Error" ? <OctagonX size={14} /> : entry.status === "Done" ? <CheckCircle2 size={14} /> : <Activity size={14} />}</span>
                          <span className="work-log-text" title={entry.text}>{entry.text}</span>
                          <time dateTime={new Date(entry.timestamp).toISOString()}>{formatTimestamp(entry.timestamp)}</time>
                        </summary>
                        {!!entry.details?.length && <div className="work-log-details">{entry.details.map((detail) => <p key={detail}>{detail}</p>)}</div>}
                      </details>
                    ) : (
                      <div className={`work-log-row ${entry.kind}`} key={entry.id} title={entry.text}>
                        <span className="work-log-icon">{entry.kind === "input" ? <MessageSquare size={14} /> : <CheckCircle2 size={14} />}</span>
                        <span className="work-log-text"><strong>{entry.kind === "input" ? "You" : "Medusa"}</strong> {entry.text}</span>
                        <time dateTime={new Date(entry.timestamp).toISOString()}>{formatTimestamp(entry.timestamp)}</time>
                      </div>
                    )
                  ))}
                </div>
                {busy && (
                  <div className="work-panel-status" role="status" aria-label={activeWorkEntry ? `Running ${activeWorkEntry.text}` : "Medusa is working"}>
                    <span>{activeWorkEntry ? activeWorkEntry.text : "Medusa is working"}</span>
                    <progress aria-label="Tool progress" />
                  </div>
                )}
                <RecoveryDock />
              </>
            )}
          </aside>
        ), sidePanelHost)}

        {activePanel === "chat" && (
          <>
            <div className="transcript" ref={transcriptRef}>
              {messages.length > transcriptLimit && (
                <button type="button" className="transcript-show-more" onClick={() => setTranscriptLimit((limit) => limit + 200)}>
                  Show {messages.length - transcriptLimit} earlier messages
                </button>
              )}
              {messages.slice(-transcriptLimit).map((message, messageIndex, visible) => (
                <article className={`message ${message.role}`} key={message.id}>
                  {!!message.text.trim() && (
                    <button
                      type="button"
                      className="message-copy-button"
                      aria-label={copiedMessageId === message.id ? "Copied message" : "Copy message"}
                      title={copiedMessageId === message.id ? "Copied" : "Copy message"}
                      onClick={() => void copyMessage(message)}
                    >
                      {copiedMessageId === message.id ? <Check size={15} /> : <Copy size={15} />}
                    </button>
                  )}
                  <div className="message-heading">
                    <span>{message.role === "user" ? "You" : "Medusa"}</span>
                    <time dateTime={new Date(message.createdAt).toISOString()}>{formatTimestamp(message.createdAt)}</time>
                    {message.failed ? <small>not sent · edit or retry</small> : message.queued && <small>queued for next turn</small>}
                  </div>
                  <div className="message-body"><MarkdownMessage text={message.text} streaming={busy && message.role === "assistant" && messageIndex === visible.length - 1} /></div>
                  {!!message.attachments?.length && (
                    <div className="message-attachments">
                      {message.attachments.map((attachment, index) => (
                        <span key={`${message.id}-${index}`}>
                          {attachment.kind === "image" ? <ImagePlus size={13} /> : <FilePlus2 size={13} />}
                          {attachment.kind === "file" ? basename(attachment.path) : attachment.name}
                        </span>
                      ))}
                    </div>
                  )}
                </article>
              ))}
              {busy && <LiveActivityTrail entries={liveActivityEntries} verboseDetails={verboseDetails} elapsedSeconds={liveElapsedSeconds} activeEntry={activeWorkEntry} />}
              {!busy && turnSummary && (
                <FinalTurnSummary
                  summary={turnSummary}
                  activities={completedActivityEntries}
                  artifact={webArtifact}
                  onPreview={() => setSidePanelView("preview")}
                  onOpenExternally={() => void openArtifactExternally()}
                />
              )}
              <div className="timeline-anchor" aria-hidden="true" />
              <ApprovalCard
                prompts={questions}
                plan={plan}
                onRespond={(response) => void sendText(response, [])}
                onEditPlan={() => {
                  markComposerEdited();
                  setPrompt("Please modify the plan: ");
                  composerRef.current?.focus();
                }}
              />
            </div>

            <footer className="composer-wrap">
              {composerSlot}
              {!!error && (
                <div className={`error-banner${hasPartialResult ? " partial" : ""}`} role={hasPartialResult ? "status" : "alert"}>
                  {hasPartialResult ? <CheckCircle2 size={15} /> : <OctagonX size={15} />}
                  <div className="error-copy">
                    <strong>{hasPartialResult ? "Medusa returned a partial result." : "Medusa couldn’t complete that request."}</strong>
                    <span>{hasPartialResult ? "The rendered result is available. Inspect Work for the failed execution step or retry the request." : "Retry the last request or inspect the technical details."}</span>
                    <details>
                      <summary>Show details</summary>
                      <code>{error}</code>
                    </details>
                  </div>
                  <button className="retry-button" onClick={() => void retryLastRequest()}>Retry</button>
                </div>
              )}
              {!!attachments.length && (
                <>
                  <div className="attachment-strip" aria-label="Attached context">
                    {attachments.map((attachment, index) => attachment.kind === "image" ? (
                      <article className="image-attachment-card" key={`${attachment.kind}-${index}`}>
                        <button className="image-preview-button" onClick={() => setPreviewImage(attachment)} aria-label={`Preview ${attachment.name}`}>
                          <img src={attachment.dataUrl} alt={attachment.name} />
                          <Maximize2 size={14} />
                        </button>
                        <div><strong>{attachment.name}</strong><small>{attachment.width && attachment.height ? `${attachment.width}×${attachment.height} · ` : ""}{attachment.mediaType?.replace("image/", "").toUpperCase()} · {formatBytes(attachment.sizeBytes)}</small></div>
                        <button className="remove-attachment" onClick={() => { markComposerEdited(); setAttachments((current) => current.filter((_, item) => item !== index)); }} aria-label={`Remove ${attachment.name}`}><X size={14} /></button>
                      </article>
                    ) : (
                      <span key={`${attachment.kind}-${index}`}>
                        <FilePlus2 size={13} />
                        {attachment.kind === "file" ? basename(attachment.path) : attachment.name}
                        <button onClick={() => { markComposerEdited(); setAttachments((current) => current.filter((_, item) => item !== index)); }} aria-label="Remove attachment"><X size={12} /></button>
                      </span>
                    ))}
                  </div>
                  {attachments.some((attachment) => attachment.kind === "image") && (
                    <div className={`image-compatibility ${imageCompatibility.supported === false ? "unsupported" : imageCompatibility.supported ? "supported" : "unknown"}`} role="status">{imageCompatibility.text}</div>
                  )}
                </>
              )}
              <div
                className={`composer-card${draggingImage ? " dragging-image" : ""}`}
                onDragEnter={(event) => { event.preventDefault(); setDraggingImage(true); }}
                onDragOver={(event) => event.preventDefault()}
                onDragLeave={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDraggingImage(false); }}
                onDrop={(event) => {
                  event.preventDefault();
                  setDraggingImage(false);
                  void addImages(Array.from(event.dataTransfer.files).filter((file) => file.type.startsWith("image/")));
                }}
              >
                {prompt.includes("@") && !slashSuggestions.length && (
                  <div className="mention-hint" role="status">Type a repo-relative path after @ to reference a file, e.g. @crates/medusa-tui/src/app.rs</div>
                )}
                {!!slashSuggestions.length && (
                  <div id="slash-command-listbox" className="slash-menu" role="listbox" aria-label="Slash command suggestions">
                    {slashSuggestions.map((suggestion, index) => (
                      <button
                        id={`slash-option-${index}`}
                        ref={(element) => { slashOptionRefs.current[index] = element; }}
                        className={`slash-row${index === slashSelection ? " active" : ""}`}
                        key={suggestion.name}
                        role="option"
                        aria-selected={index === slashSelection}
                        aria-posinset={index + 1}
                        aria-setsize={slashSuggestions.length}
                        onMouseDown={(event) => event.preventDefault()}
                        onClick={() => selectSlashSuggestion(suggestion)}
                      >
                        <span className="slash-row-label" title={suggestion.usage}>{suggestion.usage}</span><span className="slash-row-desc">{suggestion.description}</span>
                      </button>
                    ))}
                  </div>
                )}
                <div className="composer-line">
                  <div className="composer-tools">
                    {composerToolsSlot}
                    <input ref={imageInputRef} className="visually-hidden" type="file" accept="image/png,image/jpeg,image/webp,image/gif" multiple onChange={(event) => { void addImages(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
                    <button className="composer-icon-button" onClick={() => imageInputRef.current?.click()} disabled={!runtimeId} title="Add image" aria-label="Add image"><Plus size={21} /></button>
                  </div>
                  <textarea
                    ref={composerRef}
                    value={prompt}
                    disabled={!runtimeId}
                    aria-label="Message Medusa"
                    aria-autocomplete="list"
                    aria-controls={slashSuggestions.length ? "slash-command-listbox" : undefined}
                    aria-activedescendant={slashSuggestions.length ? `slash-option-${slashSelection}` : undefined}
                    onChange={(event) => { markComposerEdited(); setPrompt(event.target.value); }}
                    onPaste={onPaste}
                    onKeyDown={(event) => {
                      // Browsers dispatch Enter while an IME candidate is being
                      // confirmed. Treat it as composition input, never as send.
                      if (event.nativeEvent.isComposing || event.keyCode === 229) return;
                      if (slashSuggestions.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) { event.preventDefault(); const direction = event.key === "ArrowDown" ? 1 : -1; setSlashSelection((current) => (current + direction + slashSuggestions.length) % slashSuggestions.length); return; }
                      if (slashSuggestions.length && event.key === "Tab" && !event.shiftKey) { event.preventDefault(); selectSlashSuggestion(slashSuggestions[slashSelection]); return; }
                      if (slashSuggestions.length && event.key === "Enter" && !event.shiftKey) { const selected = slashSuggestions[slashSelection]; const exact = prompt.trim() === `/${selected.name}`; if (!exact || prompt.trim() === "/skills") { event.preventDefault(); selectSlashSuggestion(selected); return; } }
                      if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit(); }
                    }}
                    placeholder={busy ? "Add guidance for the next turn…" : repo ? "Describe a coding task…" : "Ask Medusa anything…"}
                    rows={1}
                  />
                  <div className="composer-actions">
                    <div className="composer-selector" ref={composerSelectorRef}>
                      <button
                        className="composer-selector-trigger"
                        type="button"
                        disabled={!runtimeId}
                        aria-expanded={composerSelectorOpen}
                        aria-haspopup="dialog"
                        aria-label={`Choose model and settings: ${composerSelectorLabel}`}
                        onClick={() => setComposerSelectorOpen((current) => !current)}
                      >
                        <span>{composerSelectorLabel}</span>
                        <ChevronDown size={15} aria-hidden="true" />
                      </button>
                      {composerSelectorOpen && (
                        <div className="composer-selector-popover" role="dialog" aria-modal="true" aria-labelledby="composer-selector-title">
                          <h2 id="composer-selector-title" className="visually-hidden">Provider, model, and effort</h2>
                          <label className="composer-selector-row">
                            <span>Provider</span>
                            <select
                              aria-label="Composer provider"
                              value={provider}
                              onChange={(event) => void selectProvider(event.target.value)}
                            >
                              {composerProviders.map((entry) => (
                                <option key={entry.id} value={entry.profileProvider}>
                                  {entry.displayName}
                                </option>
                              ))}
                            </select>
                          </label>
                          <label className="composer-selector-row">
                            <span>Model</span>
                            <select
                              aria-label="Composer model"
                              value={model}
                              disabled={loadingModels || !selectedProvider?.modelOptions.length}
                              onChange={(event) => setModel(event.target.value)}
                            >
                              {!selectedProvider?.modelOptions.length && <option value="">No models available</option>}
                              {(selectedProvider?.modelOptions ?? []).map((option) => (
                                <option key={option} value={option}>{option}</option>
                              ))}
                            </select>
                          </label>
                          <label className="composer-selector-row">
                            <span>Effort</span>
                            <select
                              aria-label="Composer effort"
                              value={effort}
                              onChange={(event) => setEffort(event.target.value as Effort)}
                            >
                              {effortOptions.map((option) => <option key={option} value={option}>{effortLabel(option)}</option>)}
                            </select>
                          </label>
                        </div>
                      )}
                    </div>
                    {busy && (pendingSubmit || (!prompt.trim() && attachments.length === 0)) ? (
                      <button className="send-button stop-button" onClick={() => void cancel()} aria-label="Stop active turn" title="Stop active turn"><Square size={15} /></button>
                    ) : (
                      <button className="send-button" onClick={() => void submit()} disabled={!runtimeId || (!prompt.trim() && attachments.length === 0)} aria-label="Send" title="Send"><Send size={18} /></button>
                    )}
                  </div>
                </div>
              </div>
              <div className="context-bar" role="status" aria-label="Session usage">
                <span className="context-bar-label">Context</span>
                <span className="context-bar-track" aria-hidden="true">
                  <span
                    className="context-bar-fill"
                    style={{ width: `${usage.total > 0 ? Math.min(100, Math.round((usage.cached / Math.max(1, usage.total)) * 100)) : 0}%` }}
                  />
                </span>
                <span className="context-bar-stats">{usage.input.toLocaleString()} in · {usage.output.toLocaleString()} out · {usage.cached.toLocaleString()} cached</span>
              </div>
            </footer>
          </>
        )}

        {activePanel === "plan" && (
          <div className="standalone-panel">
            <div className="panel-title"><ListChecks size={18} /><div><h2>Execution plan</h2><p>Live plan state from medusa-runtime</p></div></div>
            {plan.length ? plan.map((step) => <div className={`plan-row ${step.status}`} key={step.title}>{planIcon(step.status)}<span>{step.title}</span></div>) : <p className="muted-copy">No plan has been created for this session.</p>}
            <button className="secondary-action" disabled={!runtimeId} onClick={() => void sendText("/plan", [])}>Enter plan mode</button>
          </div>
        )}

        {activePanel === "settings" && (
          <div className="standalone-panel settings-form">
            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Credentials are retained in the appropriate local credential store</p><small>Shared profile: {sharedConfiguration?.activeProfile ?? "loading"} · revision {sharedConfiguration?.revision ?? "…"}</small></div></div>
            <label>Provider<select value={provider} onChange={(event) => void selectProvider(event.target.value)}>{providerCatalog.map((entry) => <option key={entry.id} value={entry.profileProvider} disabled={Boolean(entry.disabledReason)}>{entry.displayName}{entry.currentCustom ? " (current custom)" : ""}</option>)}</select></label>
            {selectedProvider && <small>{selectedProvider.disabledReason ?? selectedProvider.description}</small>}
            {loadingModels && <small role="status">Refreshing available models…</small>}
            <label>Model<select value={model} disabled={loadingModels} onChange={(event) => setModel(event.target.value)}>{(selectedProvider?.modelOptions ?? (model ? [model] : [])).map((option) => <option key={option} value={option}>{option}</option>)}</select></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}>{effortOptions.map((option) => <option key={option} value={option}>{effortLabel(option)}</option>)}</select></label>
            {oauthProvider ? (
              <div>
                <button className="secondary-action" disabled={authenticating} onClick={() => void authenticateSelectedProvider()}>{authenticating ? "Opening ChatGPT sign-in…" : oauthAuthenticated ? "Re-authenticate with ChatGPT" : "Sign in with ChatGPT"}</button>
                <small>{oauthAuthenticated ? "ChatGPT OAuth credential found. Apply configuration to verify the route and selected model." : "Authenticate your ChatGPT subscription in the browser before applying this provider."}</small>
              </div>
            ) : (
              <label>API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder={credentiallessProvider ? "This route does not require an API key" : "Leave blank to use the saved key"} disabled={credentiallessProvider} /></label>
            )}
            {selectedProvider?.customValues && <label>Base URL<input type="url" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/v1" /></label>}
            {selectedProvider && <small>Route: {selectedProvider.customValues ? baseUrl || "not set" : selectedProvider.baseUrl ?? "provider default"}. Medusa verifies the endpoint and selected model before applying.</small>}
            <button className="primary-action" onClick={applyModel} disabled={!runtimeId || !sharedConfiguration || !provider.trim() || !model.trim() || loadingModels || authenticating || (oauthProvider && !oauthAuthenticated) || Boolean(selectedProvider?.disabledReason)}>Apply configuration</button>
            {settingsSlot}
          </div>
        )}
      </section>

      <div ref={setSidePanelHost} className="side-panel-slot" aria-hidden={!sidePanelView} />

    </main>
      {previewImage && (
        <div className="image-preview-modal" role="dialog" aria-modal="true" aria-labelledby="preview-image-title" onClick={closePreviewImage}>
          <div ref={previewDialogRef} className="image-preview-content" tabIndex={-1} onClick={(event) => event.stopPropagation()}>
            <div><strong id="preview-image-title">{previewImage.name}</strong><small>{previewImage.width}×{previewImage.height} · {formatBytes(previewImage.sizeBytes)}</small></div>
            <button onClick={closePreviewImage} aria-label="Close image preview"><X size={18} /></button>
            <img src={previewImage.dataUrl} alt={previewImage.name} />
          </div>
        </div>
      )}
    </>
  );
}
