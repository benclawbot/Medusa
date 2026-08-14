import {
  Activity,
  BarChart3,
  Bot,
  Brain,
  CheckCircle2,
  ChevronRight,
  Circle,
  FilePlus2,
  FolderOpen,
  Gauge,
  GitCompareArrows,
  GraduationCap,
  History,
  ImagePlus,
  Info,
  ListChecks,
  Maximize2,
  MessageSquare,
  OctagonX,
  PanelLeftClose,
  PanelLeftOpen,
  Play,
  Plus,
  Send,
  Settings,
  ShieldCheck,
  Square,
  X,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApprovalCard } from "./ApprovalCard";
import { DesktopOnboarding } from "./DesktopOnboarding";
import "./approval-card.css";
import { loadProviderCatalog, type ProviderCatalogEntry } from "./providerCatalog";
import {
  cancelRuntime,
  commandSuggestions,
  closeRuntime,
  configureRuntime,
  loadSharedConfiguration,
  pollRuntime,
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
} from "./runtime";

interface ConversationMessage {
  id: number;
  role: "user" | "assistant" | "system";
  text: string;
  attachments?: DesktopAttachment[];
  queued?: boolean;
}

interface UsageState {
  input: number;
  output: number;
  cached: number;
  cacheWrite: number;
  elapsed: number;
}

interface SettingsState {
  model: string;
  effort: string;
  planMode: boolean;
  credentialConfigured: boolean;
}

const emptyUsage: UsageState = { input: 0, output: 0, cached: 0, cacheWrite: 0, elapsed: 0 };
let messageCounter = 0;
const nextMessageId = () => ++messageCounter;
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
const SUPPORTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/webp", "image/gif"]);

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

function ConversationText({ text }: { text: string }) {
  const urlPattern = /https?:\/\/[^\s]+/g;
  const parts: React.ReactNode[] = [];
  let cursor = 0;
  for (const match of text.matchAll(urlPattern)) {
    const start = match.index ?? 0;
    const raw = match[0];
    const url = raw.replace(/[.,;:!?\)\]\}]+$/, "");
    parts.push(text.slice(cursor, start));
    parts.push(
      <a
        key={`${start}-${url}`}
        href={url}
        target="_blank"
        rel="noreferrer"
        title="Ctrl+click to open"
        onClick={(event) => {
          if (!event.ctrlKey) event.preventDefault();
        }}
      >
        {url}
      </a>,
    );
    parts.push(raw.slice(url.length));
    cursor = start + raw.length;
  }
  parts.push(text.slice(cursor));
  return <>{parts}</>;
}

async function configureStartedRuntime(
  started: Awaited<ReturnType<typeof startRuntime>>,
  configuration: {
    provider: string;
    model: string;
    effort: Effort;
    expectedRevision: number;
  },
): Promise<Awaited<ReturnType<typeof startRuntime>>> {
  try {
    await configureRuntime(started.runtimeId, configuration);
    return started;
  } catch (cause) {
    try {
      await closeRuntime(started.runtimeId);
    } catch (cleanupCause) {
      throw new Error(
        `Runtime configuration failed (${String(cause)}); cleanup also failed (${String(cleanupCause)}).`,
      );
    }
    throw cause;
  }
}

export function App() {
  const [runtimeId, setRuntimeId] = useState<string>();
  const [repo, setRepo] = useState("");
  const [messages, setMessages] = useState<ConversationMessage[]>([]);
  const [activities, setActivities] = useState<RuntimeActivity[]>([]);
  const [plan, setPlan] = useState<PlanStep[]>([]);
  const [questions, setQuestions] = useState<QuestionPrompt[]>([]);
  const [usage, setUsage] = useState<UsageState>(emptyUsage);
  const [settings, setSettings] = useState<SettingsState>({
    model: "not connected",
    effort: "effort:auto",
    planMode: false,
    credentialConfigured: false,
  });
  const [prompt, setPrompt] = useState("");
  const [slashSuggestions, setSlashSuggestions] = useState<CommandSuggestion[]>([]);
  const [slashSelection, setSlashSelection] = useState(0);
  const [attachments, setAttachments] = useState<DesktopAttachment[]>([]);
  const [previewImage, setPreviewImage] = useState<Extract<DesktopAttachment, { kind: "image" }>>();
  const [draggingImage, setDraggingImage] = useState(false);
  const [busy, setBusy] = useState(false);
  const [turn, setTurn] = useState(0);
  const [error, setError] = useState<string>();
  const [provider, setProvider] = useState("");
  const [providerCatalog, setProviderCatalog] = useState<ProviderCatalogEntry[]>([]);
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState<Effort>("medium");
  const [sharedConfiguration, setSharedConfiguration] = useState<SharedConfiguration>();
  const [apiKey, setApiKey] = useState("");
  const [activePanel, setActivePanel] = useState<"chat" | "plan" | "settings">("chat");
  const [railCollapsed, setRailCollapsed] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const pollBusy = useRef(false);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const imageInputRef = useRef<HTMLInputElement>(null);

  const refreshConfiguration = useCallback(async () => {
    const [configuration, catalog] = await Promise.all([
      loadSharedConfiguration(),
      loadProviderCatalog(),
    ]);
    setSharedConfiguration(configuration);
    setProviderCatalog(catalog);
    setProvider(configuration.provider);
    setModel(configuration.model);
    setEffort(configuration.effort);
    return configuration;
  }, []);

  const applyEvent = useCallback((event: RuntimeEvent) => {
    switch (event.type) {
      case "started":
        setBusy(true);
        setError(undefined);
        break;
      case "assistantText":
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "assistant", text: event.text },
        ]);
        break;
      case "activity":
        setActivities((current) => {
          if (!event.activity.id) return [...current, event.activity];
          const index = current.findIndex((item) => item.id === event.activity.id);
          if (index < 0) return [...current, event.activity];
          const next = [...current];
          next[index] = event.activity;
          return next;
        });
        break;
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
          elapsed: event.modelElapsedMillis,
        });
        break;
      case "progress":
        setTurn(event.turn);
        break;
      case "settings":
        setSettings({
          model: event.model,
          effort: event.effort,
          planMode: event.planMode,
          credentialConfigured: event.credentialConfigured,
        });
        break;
      case "configurationChanged":
        void refreshConfiguration().catch((cause) => setError(String(cause)));
        break;
      case "notice":
        setMessages((current) => [
          ...current,
          {
            id: nextMessageId(),
            role: "system",
            text: [event.title, ...event.details].join("\n"),
          },
        ]);
        break;
      case "newSession":
        setMessages([]);
        setActivities([]);
        setPlan([]);
        setQuestions([]);
        setUsage(emptyUsage);
        setTurn(0);
        setBusy(false);
        break;
      case "compacted":
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "system", text: event.message },
        ]);
        break;
      case "completed":
        setBusy(false);
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "system", text: `Session ${event.sessionId} completed.` },
        ]);
        break;
      case "turnFinished":
        setBusy(false);
        break;
      case "cancelled":
        setBusy(false);
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "system", text: "The active turn was cancelled." },
        ]);
        break;
      case "failed":
        setBusy(false);
        setError(event.message);
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "system", text: `Runtime failed: ${event.message}` },
        ]);
        break;
    }
  }, [refreshConfiguration]);

  useEffect(() => {
    const transcript = transcriptRef.current;
    if (transcript && typeof transcript.scrollTo === "function") {
      transcript.scrollTo({ top: transcript.scrollHeight, behavior: "smooth" });
    }
  }, [messages, activities]);

  useEffect(() => {
    if (!runtimeId) return;
    let active = true;
    const interval = window.setInterval(async () => {
      if (!active || pollBusy.current) return;
      pollBusy.current = true;
      try {
        const events = await pollRuntime(runtimeId);
        events.forEach(applyEvent);
      } catch (cause) {
        if (active) setError(String(cause));
      } finally {
        pollBusy.current = false;
      }
    }, 120);
    return () => {
      active = false;
      window.clearInterval(interval);
    };
  }, [runtimeId, applyEvent]);

  useEffect(() => {
    if (!runtimeId || !prompt.trimStart().startsWith("/") || prompt.includes("\n")) {
      setSlashSuggestions([]);
      return;
    }
    let active = true;
    void commandSuggestions(runtimeId, prompt)
      .then((suggestions) => {
        if (!active) return;
        setSlashSuggestions(suggestions);
        setSlashSelection(0);
      })
      .catch((cause) => {
        if (active) setError(String(cause));
      });
    return () => {
      active = false;
    };
  }, [runtimeId, prompt]);

  useEffect(() => {
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    let disposed = false;
    const start = async () => {
      const configuration = await refreshConfiguration();
      if (!configuration.configured || !configuration.provider.trim() || !configuration.model.trim()) return undefined;
      let started;
      try {
        started = await startRuntime(previous || undefined);
      } catch (cause) {
        if (!previous) throw cause;
        window.localStorage.removeItem("medusa.desktop.repo");
        started = await startRuntime();
      }
      return configureStartedRuntime(started, {
        provider: configuration.provider,
        model: configuration.model,
        effort: configuration.effort,
        expectedRevision: configuration.revision,
      });
    };
    void start()
      .then((started) => {
        if (!started) return;
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
      })
      .catch((cause) => {
        if (!disposed) setError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, [refreshConfiguration]);

  useEffect(() => () => {
    if (runtimeId) void closeRuntime(runtimeId);
  }, [runtimeId]);

  const openProject = async () => {
    const selected = await open({ directory: true, multiple: false, title: "Open a Medusa project" });
    if (typeof selected !== "string") return;
    try {
      const started = await configureStartedRuntime(await startRuntime(selected), {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
      });
      await refreshConfiguration();
      if (runtimeId) await closeRuntime(runtimeId);
      setRuntimeId(started.runtimeId);
      setRepo(started.repo);
      setMessages([]);
      setActivities([]);
      setPlan([]);
      setQuestions([]);
      setError(undefined);
      window.localStorage.setItem("medusa.desktop.repo", started.repo);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const openGeneralChat = async () => {
    try {
      const started = await configureStartedRuntime(await startRuntime(), {
        provider,
        model,
        effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
      });
      await refreshConfiguration();
      if (runtimeId) await closeRuntime(runtimeId);
      setRuntimeId(started.runtimeId);
      setRepo("");
      setMessages([]);
      setActivities([]);
      setPlan([]);
      setQuestions([]);
      setError(undefined);
      window.localStorage.removeItem("medusa.desktop.repo");
    } catch (cause) {
      setError(String(cause));
    }
  };

  const addFiles = async () => {
    if (!repo) return;
    const selected = await open({ multiple: true, directory: false, title: "Attach repository files" });
    const paths = typeof selected === "string" ? [selected] : selected ?? [];
    setAttachments((current) => [
      ...current,
      ...paths.map((path): DesktopAttachment => ({ kind: "file", path })),
    ]);
  };


  const addImages = async (files: File[]) => {
    if (!files.length) return;
    try {
      const next = await Promise.all(files.map(readImage));
      setAttachments((current) => [...current, ...next]);
      setError(undefined);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const imageCompatibility = useMemo(() => {
    const normalized = provider.trim().toLowerCase();
    if (normalized === "anthropic" || normalized === "openai" || normalized === "chatgpt-oauth") {
      return { supported: true, text: `${model} is configured for image input.` };
    }
    if (normalized === "anthropic-compatible") {
      return { supported: false, text: "This compatible route is text-only unless image support is explicitly configured." };
    }
    return { supported: undefined, text: "Image compatibility will be verified by the runtime before upload." };
  }, [provider, model]);

  const onPaste = async (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const images = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"));
    if (!images.length) return;
    event.preventDefault();
    await addImages(images);
  };

  const sendText = async (text: string, suppliedAttachments = attachments) => {
    if (!runtimeId || (!text.trim() && suppliedAttachments.length === 0)) return;
    if (suppliedAttachments.some((attachment) => attachment.kind === "image") && imageCompatibility.supported === false) {
      setError(`${imageCompatibility.text} Switch model/route or remove the image.`);
      return;
    }
    const clean = text.trim();
    setError(undefined);
    setQuestions([]);
    try {
      if (clean.startsWith("/") && suppliedAttachments.length === 0) {
        await runRuntimeCommand(runtimeId, clean);
        setMessages((current) => [
          ...current,
          { id: nextMessageId(), role: "user", text: clean },
        ]);
      } else {
        const disposition = await submitRuntime(runtimeId, {
          text,
          attachments: suppliedAttachments,
          revision: Date.now(),
        });
        setMessages((current) => [
          ...current,
          {
            id: nextMessageId(),
            role: "user",
            text: text || "Attached context",
            attachments: suppliedAttachments,
            queued: disposition === "queued",
          },
        ]);
        setBusy(true);
      }
      setPrompt("");
      setAttachments([]);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const submit = async () => sendText(prompt);

  const selectSlashSuggestion = (suggestion: CommandSuggestion) => {
    setPrompt(`/${suggestion.name} `);
    setSlashSuggestions([]);
    setSlashSelection(0);
  };

  const completeOnboarding = async (next: { provider: string; model: string; effort: Effort; apiKey?: string }) => {
    const started = await startRuntime();
    try {
      await configureRuntime(started.runtimeId, {
        provider: next.provider,
        model: next.model,
        effort: next.effort,
        expectedRevision: sharedConfiguration?.revision ?? 0,
        apiKey: next.apiKey,
      });
      const configuration = await refreshConfiguration();
      setProvider(configuration.provider);
      setModel(configuration.model);
      setEffort(configuration.effort);
      setRuntimeId(started.runtimeId);
      setRepo(started.repo);
      setError(undefined);
    } catch (cause) {
      await closeRuntime(started.runtimeId).catch(() => undefined);
      setError(String(cause));
      throw cause;
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
      });
      await refreshConfiguration();
      setApiKey("");
      setError(undefined);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const cancel = async () => {
    if (!runtimeId) return;
    try {
      await cancelRuntime(runtimeId);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const newSession = useCallback(async () => {
    if (!runtimeId) return;
    try {
      await runRuntimeCommand(runtimeId, "/new");
    } catch (cause) {
      setError(String(cause));
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
  const credentiallessProvider = selectedProvider?.authMethods.every((method) => method === "none") ?? false;
  const repoName = useMemo(() => basename(repo) || "General chat", [repo]);
  const totalTokens = usage.input + usage.output;
  const openDesktopTool = (selector: string) => {
    document.querySelector<HTMLButtonElement>(selector)?.click();
  };

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
    <main className={`app-shell medusa-shell${railCollapsed ? " rail-collapsed" : ""}`}>
      <aside className="sidebar" aria-label="Session rail">
        <div className="window-dots" aria-hidden="true">
          <span className="dot red" /><span className="dot yellow" /><span className="dot green" />
        </div>
        <div className="brand-row">
          <span className="brand-mark"><Bot size={17} /></span>
          <div className="rail-label"><h1>Medusa</h1><small>Desktop</small></div>
          <span className="version rail-label">v1.0</span>
        </div>
        <button className="new-session" onClick={newSession} disabled={!runtimeId} aria-keyshortcuts={macPlatform ? "Meta+N" : "Control+N"} title="New session">
          <span><Plus size={16} /><span className="rail-label">New session</span></span><kbd className="rail-label">{newSessionShortcut}</kbd>
        </button>
        <nav className="nav-list" aria-label="Workspace views">
          <button className={`nav-item ${activePanel === "chat" ? "active" : ""}`} onClick={() => setActivePanel("chat")} title="Chat">
            <MessageSquare size={17} /><span className="rail-label">Chat</span>
          </button>
          <button className={`nav-item ${activePanel === "plan" ? "active" : ""}`} onClick={() => setActivePanel("plan")} title="Plan">
            <ListChecks size={17} /><span className="rail-label">Plan</span>
          </button>
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
          <p className="section-label rail-label">Tools</p>
          <button className="nav-item" onClick={() => openDesktopTool(".session-dock-trigger")} title="Sessions"><History size={17} /><span className="rail-label">Sessions</span></button>
          <button className="nav-item" onClick={() => openDesktopTool(".diff-dock-trigger")} title="Review changes"><GitCompareArrows size={17} /><span className="rail-label">Review changes</span></button>
          <button className="nav-item" onClick={() => openDesktopTool(".memory-dock-trigger")} title="Memory"><Brain size={17} /><span className="rail-label">Memory</span></button>
          <button className="nav-item" onClick={() => openDesktopTool(".learning-launcher")} title="Learning"><GraduationCap size={17} /><span className="rail-label">Learning</span></button>
          <button className="nav-item" onClick={() => openDesktopTool(".engineering-menu-button")} title="Engineering"><BarChart3 size={17} /><span className="rail-label">Engineering</span></button>
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
            <button className="details-button" onClick={() => setDetailsOpen((current) => !current)} aria-expanded={detailsOpen} aria-controls="session-details-panel">
              <Info size={16} /> Session details
            </button>
            <div className="runtime-state" role="status">
              <span className={`status-dot ${busy ? "busy" : runtimeId ? "ready" : "offline"}`} />
              {busy ? `Working · turn ${turn}` : runtimeId ? "Ready" : "Starting"}
            </div>
          </div>
        </header>

        {detailsOpen && (
          <aside id="session-details-panel" className="session-details-panel" role="complementary" aria-label="Session details">
            <div className="session-details-heading"><div><p className="eyebrow">Progressive disclosure</p><h2>Session details</h2></div><button className="icon-button" onClick={() => setDetailsOpen(false)} aria-label="Close session details"><X size={17} /></button></div>
            <section className="details-section">
              <div className="panel-heading"><span><Gauge size={15} /> Runtime</span></div>
              <dl className="metric-grid">
                <div><dt>Model</dt><dd>{settings.model}</dd></div>
                <div><dt>Effort</dt><dd>{settings.effort.replace("effort:", "")}</dd></div>
                <div><dt>Mode</dt><dd>{settings.planMode ? "Plan" : "Full"}</dd></div>
                <div><dt>Credential</dt><dd>{settings.credentialConfigured ? "Ready" : "Missing"}</dd></div>
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
          </aside>
        )}

        {activePanel === "chat" && (
          <>
            <div className="transcript" ref={transcriptRef}>
              {!runtimeId && (
                <div className="empty-state">
                  <span className="empty-icon"><Bot size={28} /></span>
                  <h2>Starting Medusa</h2>
                  <p>Preparing a general chat. You can attach a project whenever the task needs repository access.</p>
                </div>
              )}
              {runtimeId && messages.length === 0 && (
                <div className="empty-state compact">
                  <h2>{repo ? "What should Medusa build?" : "How can Medusa help?"}</h2>
                  <p>{repo ? "Describe a coding task, paste a screenshot, attach repository files, or use a slash command." : "Ask a question, paste a screenshot, or open a project when you want Medusa to work on files."}</p>
                </div>
              )}
              {messages.map((message) => (
                <article className={`message ${message.role}`} key={message.id}>
                  <div className="message-heading">
                    <span>{message.role === "user" ? "You" : message.role === "assistant" ? "Medusa" : "Runtime"}</span>
                    {message.queued && <small>queued for next turn</small>}
                  </div>
                  <div className="message-body"><ConversationText text={message.text} /></div>
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
              {!!activities.length && (
                <section className="activity-summary" aria-label="Tool activity">
                  <div className="activity-summary-heading"><span><Activity size={15} /> Activity</span><small>{activities.length} update{activities.length === 1 ? "" : "s"}</small></div>
                  {activities.slice(-4).map((item, index) => (
                    <details className={`activity-row ${item.kind}`} key={item.id ?? `${item.title}-${index}`}>
                      <summary><span>{item.kind === "error" ? <OctagonX size={14} /> : item.kind === "done" ? <CheckCircle2 size={14} /> : <Activity size={14} />}</span><strong>{item.title}</strong><small>{item.kind === "done" ? "Done" : item.kind === "error" ? "Error" : "Working"}</small></summary>
                      {!!item.details.length && <div className="activity-details">{item.details.map((detail) => <p key={detail}>{detail}</p>)}</div>}
                    </details>
                  ))}
                </section>
              )}
              <ApprovalCard
                prompts={questions}
                plan={plan}
                onRespond={(response) => void sendText(response, [])}
                onEditPlan={() => {
                  setPrompt("Please modify the plan: ");
                  composerRef.current?.focus();
                }}
              />
              {busy && <div className="thinking-row"><Activity size={15} /> Medusa is working…</div>}
            </div>

            <footer className="composer-wrap">
              {!!error && <div className="error-banner" role="alert"><OctagonX size={15} /><span>{error}</span><button className="retry-button" onClick={() => { setError(undefined); composerRef.current?.focus(); }}>Retry</button></div>}
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
                        <button className="remove-attachment" onClick={() => setAttachments((current) => current.filter((_, item) => item !== index))} aria-label={`Remove ${attachment.name}`}><X size={14} /></button>
                      </article>
                    ) : (
                      <span key={`${attachment.kind}-${index}`}>
                        <FilePlus2 size={13} />
                        {attachment.kind === "file" ? basename(attachment.path) : attachment.name}
                        <button onClick={() => setAttachments((current) => current.filter((_, item) => item !== index))} aria-label="Remove attachment"><X size={12} /></button>
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
                {!!slashSuggestions.length && (
                  <div className="slash-menu" role="listbox" aria-label="Slash commands">
                    {slashSuggestions.map((suggestion, index) => (
                      <button className={`slash-row${index === slashSelection ? " active" : ""}`} key={suggestion.name} role="option" aria-selected={index === slashSelection} onMouseDown={(event) => event.preventDefault()} onClick={() => selectSlashSuggestion(suggestion)}>
                        <span className="slash-row-label">{suggestion.usage}</span><span className="slash-row-desc">{suggestion.description}</span><span className="slash-row-kind">{prompt.startsWith("/skills ") ? "skill" : "command"}</span>
                      </button>
                    ))}
                  </div>
                )}
                <textarea
                  ref={composerRef}
                  value={prompt}
                  disabled={!runtimeId}
                  onChange={(event) => setPrompt(event.target.value)}
                  onPaste={onPaste}
                  onKeyDown={(event) => {
                    if (slashSuggestions.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) { event.preventDefault(); const direction = event.key === "ArrowDown" ? 1 : -1; setSlashSelection((current) => (current + direction + slashSuggestions.length) % slashSuggestions.length); return; }
                    if (slashSuggestions.length && event.key === "Tab" && !event.shiftKey) { event.preventDefault(); selectSlashSuggestion(slashSuggestions[slashSelection]); return; }
                    if (slashSuggestions.length && event.key === "Enter" && !event.shiftKey) { const selected = slashSuggestions[slashSelection]; const exact = prompt.trim() === `/${selected.name}`; if (!exact || prompt.trim() === "/skills") { event.preventDefault(); selectSlashSuggestion(selected); return; } }
                    if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit(); }
                  }}
                  placeholder={runtimeId ? busy ? "Add guidance for the next turn…" : repo ? "Describe a coding task…" : "Ask Medusa anything…" : "Starting Medusa…"}
                  rows={3}
                />
                <div className="composer-bottom">
                  <div className="composer-tools">
                    <input ref={imageInputRef} className="visually-hidden" type="file" accept="image/png,image/jpeg,image/webp,image/gif" multiple onChange={(event) => { void addImages(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
                    <button className="composer-icon-button" onClick={() => imageInputRef.current?.click()} disabled={!runtimeId} title="Add image" aria-label="Add image"><Plus size={21} /></button>
                    <button className="composer-icon-button" onClick={addFiles} disabled={!runtimeId || !repo} title="Attach project files" aria-label="Attach project files"><FilePlus2 size={19} /></button>
                    <span className="composer-hint">Shift+Enter for a new line</span>
                  </div>
                  <div className="composer-actions">
                    {busy && <button className="cancel-button" onClick={cancel} aria-label="Stop active turn"><Square size={13} /> Stop</button>}
                    <button className="send-button" onClick={submit} disabled={!runtimeId || (!prompt.trim() && attachments.length === 0)} aria-label="Send"><Send size={18} /></button>
                  </div>
                </div>
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
            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Saved securely in your operating system credential manager</p><small>Shared profile: {sharedConfiguration?.activeProfile ?? "loading"} · revision {sharedConfiguration?.revision ?? "…"}</small></div></div>
            <label>Provider<select value={provider} onChange={(event) => {
              const nextProvider = providerCatalog.find((entry) => entry.profileProvider === event.target.value);
              setProvider(event.target.value);
              if (nextProvider) {
                setModel(nextProvider.defaultModel);
                setApiKey("");
              }
            }}>{providerCatalog.map((entry) => <option key={entry.id} value={entry.profileProvider} disabled={Boolean(entry.disabledReason)}>{entry.displayName}{entry.currentCustom ? " (current custom)" : ""}</option>)}</select></label>
            {selectedProvider && <small>{selectedProvider.disabledReason ?? selectedProvider.description}</small>}
            <label>Model<select value={model} onChange={(event) => setModel(event.target.value)}>{(selectedProvider?.modelOptions ?? (model ? [model] : [])).map((option) => <option key={option} value={option}>{option}</option>)}</select></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}><option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
            <label>API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder={credentiallessProvider ? "This route does not require an API key" : "Leave blank to use the saved key"} disabled={credentiallessProvider} /></label>
            <button className="primary-action" onClick={applyModel} disabled={!runtimeId || !sharedConfiguration || !provider.trim() || !model.trim() || Boolean(selectedProvider?.disabledReason)}>Apply configuration</button>
          </div>
        )}
      </section>
    </main>
      {previewImage && (
        <div className="image-preview-modal" role="dialog" aria-modal="true" aria-label={`Preview ${previewImage.name}`} onClick={() => setPreviewImage(undefined)}>
          <div className="image-preview-content" onClick={(event) => event.stopPropagation()}>
            <div><strong>{previewImage.name}</strong><small>{previewImage.width}×{previewImage.height} · {formatBytes(previewImage.sizeBytes)}</small></div>
            <button onClick={() => setPreviewImage(undefined)} aria-label="Close image preview"><X size={18} /></button>
            <img src={previewImage.dataUrl} alt={previewImage.name} />
          </div>
        </div>
      )}
    </>
  );
}
