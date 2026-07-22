import {
  Activity,
  Bot,
  CheckCircle2,
  ChevronRight,
  Circle,
  FilePlus2,
  FolderOpen,
  Gauge,
  ImagePlus,
  ListChecks,
  MessageSquare,
  OctagonX,
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
import "./approval-card.css";
import {
  cancelRuntime,
  commandSuggestions,
  closeRuntime,
  configureRuntime,
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

interface ModelPreferences {
  provider: string;
  model: string;
  effort: Effort;
}

const emptyUsage: UsageState = { input: 0, output: 0, cached: 0, cacheWrite: 0, elapsed: 0 };
const defaultModelPreferences: ModelPreferences = {
  provider: "minimax",
  model: "MiniMax-M2.5",
  effort: "auto",
};
let messageCounter = 0;
const nextMessageId = () => ++messageCounter;

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function loadModelPreferences(): ModelPreferences {
  const raw = window.localStorage.getItem("medusa.desktop.model");
  if (!raw) return defaultModelPreferences;
  try {
    const value = JSON.parse(raw) as Partial<ModelPreferences>;
    if (
      typeof value.provider === "string" && value.provider.trim() &&
      typeof value.model === "string" && value.model.trim() &&
      ["auto", "low", "medium", "high"].includes(value.effort ?? "")
    ) {
      return value as ModelPreferences;
    }
  } catch {
    window.localStorage.removeItem("medusa.desktop.model");
  }
  return defaultModelPreferences;
}

function readImage(file: File): Promise<DesktopAttachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== "string") {
        reject(new Error("The pasted image could not be read."));
        return;
      }
      resolve({ kind: "image", name: file.name || "pasted-image.png", dataUrl: reader.result });
    };
    reader.onerror = () => reject(new Error("The pasted image could not be read."));
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

export function App() {
  const modelPreferences = useMemo(loadModelPreferences, []);
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
  const [busy, setBusy] = useState(false);
  const [turn, setTurn] = useState(0);
  const [error, setError] = useState<string>();
  const [provider, setProvider] = useState(modelPreferences.provider);
  const [model, setModel] = useState(modelPreferences.model);
  const [effort, setEffort] = useState<Effort>(modelPreferences.effort);
  const [apiKey, setApiKey] = useState("");
  const [activePanel, setActivePanel] = useState<"chat" | "plan" | "settings">("chat");
  const pollBusy = useRef(false);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);

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
  }, []);

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
      try {
        return await startRuntime(previous || undefined);
      } catch (cause) {
        if (!previous) throw cause;
        window.localStorage.removeItem("medusa.desktop.repo");
        return startRuntime();
      }
    };
    void start()
      .then((started) => {
        if (disposed) {
          void closeRuntime(started.runtimeId);
          return;
        }
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
        void configureRuntime(started.runtimeId, modelPreferences).catch((cause) => {
          if (!disposed) setError(String(cause));
        });
      })
      .catch((cause) => {
        if (!disposed) setError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, []);

  useEffect(() => () => {
    if (runtimeId) void closeRuntime(runtimeId);
  }, [runtimeId]);

  const openProject = async () => {
    const selected = await open({ directory: true, multiple: false, title: "Open a Medusa project" });
    if (typeof selected !== "string") return;
    try {
      const started = await startRuntime(selected);
      await configureRuntime(started.runtimeId, { provider, model, effort });
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
      const started = await startRuntime();
      await configureRuntime(started.runtimeId, { provider, model, effort });
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

  const onPaste = async (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const images = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"));
    if (!images.length) return;
    event.preventDefault();
    try {
      const next = await Promise.all(images.map(readImage));
      setAttachments((current) => [...current, ...next]);
    } catch (cause) {
      setError(String(cause));
    }
  };

  const sendText = async (text: string, suppliedAttachments = attachments) => {
    if (!runtimeId || (!text.trim() && suppliedAttachments.length === 0)) return;
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

  const applyModel = async () => {
    if (!runtimeId) return;
    try {
      await configureRuntime(runtimeId, {
        provider,
        model,
        effort,
        apiKey: apiKey.trim() || undefined,
      });
      window.localStorage.setItem(
        "medusa.desktop.model",
        JSON.stringify({ provider, model, effort }),
      );
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

  const newSession = async () => {
    if (!runtimeId) return;
    try {
      await runRuntimeCommand(runtimeId, "/new");
    } catch (cause) {
      setError(String(cause));
    }
  };

  const repoName = useMemo(() => basename(repo) || "General chat", [repo]);
  const totalTokens = usage.input + usage.output;

  return (
    <main className="app-shell medusa-shell">
      <aside className="sidebar">
        <div className="window-dots" aria-hidden="true">
          <span className="dot red" /><span className="dot yellow" /><span className="dot green" />
        </div>
        <div className="brand-row">
          <span className="brand-mark"><Bot size={17} /></span>
          <div><h1>Medusa</h1><small>Desktop</small></div>
          <span className="version">v1.0</span>
        </div>
        <button className="new-session" onClick={newSession} disabled={!runtimeId}>
          <span><Plus size={15} /> New session</span><kbd>⌘N</kbd>
        </button>
        <nav className="nav-list" aria-label="Workspace views">
          <button className={`nav-item ${activePanel === "chat" ? "active" : ""}`} onClick={() => setActivePanel("chat")}>
            <MessageSquare size={16} /> Chat
          </button>
          <button className={`nav-item ${activePanel === "plan" ? "active" : ""}`} onClick={() => setActivePanel("plan")}>
            <ListChecks size={16} /> Plan
          </button>
          <button className={`nav-item ${activePanel === "settings" ? "active" : ""}`} onClick={() => setActivePanel("settings")}>
            <Settings size={16} /> Settings
          </button>
        </nav>
        <section className="project-card">
          <p className="section-label">Context</p>
          <button className="project-picker" onClick={openProject}>
            <FolderOpen size={16} />
            <span><strong>{repoName}</strong><small>{repo || "No project attached"}</small></span>
            <ChevronRight size={15} />
          </button>
          {!!repo && <button className="projectless-action" onClick={openGeneralChat}>Switch to general chat</button>}
        </section>
        <div className="sidebar-spacer" />
        <div className="security-note"><ShieldCheck size={15} /> Medusa policy remains authoritative</div>
      </aside>

      <section className="workspace medusa-workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">{activePanel === "chat" ? "Interactive session" : activePanel}</p>
            <h2>{repoName}</h2>
          </div>
          <div className="runtime-state">
            <span className={`status-dot ${busy ? "busy" : runtimeId ? "ready" : "offline"}`} />
            {busy ? `Working · turn ${turn}` : runtimeId ? "Ready" : "Starting"}
          </div>
        </header>

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
              {!!error && <div className="error-banner"><OctagonX size={15} /> {error}</div>}
              {!!attachments.length && (
                <div className="attachment-strip">
                  {attachments.map((attachment, index) => (
                    <span key={`${attachment.kind}-${index}`}>
                      {attachment.kind === "image" ? <ImagePlus size={13} /> : <FilePlus2 size={13} />}
                      {attachment.kind === "file" ? basename(attachment.path) : attachment.name}
                      <button onClick={() => setAttachments((current) => current.filter((_, item) => item !== index))} aria-label="Remove attachment"><X size={12} /></button>
                    </span>
                  ))}
                </div>
              )}
              <div className="composer-card">
                {!!slashSuggestions.length && (
                  <div className="slash-menu" role="listbox" aria-label="Slash commands">
                    {slashSuggestions.map((suggestion, index) => (
                      <button
                        className={`slash-row${index === slashSelection ? " active" : ""}`}
                        key={suggestion.name}
                        role="option"
                        aria-selected={index === slashSelection}
                        onMouseDown={(event) => event.preventDefault()}
                        onClick={() => selectSlashSuggestion(suggestion)}
                      >
                        <span className="slash-row-label">{suggestion.usage}</span>
                        <span className="slash-row-desc">{suggestion.description}</span>
                        <span className="slash-row-kind">{prompt.startsWith("/skills ") ? "skill" : "command"}</span>
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
                    if (slashSuggestions.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
                      event.preventDefault();
                      const direction = event.key === "ArrowDown" ? 1 : -1;
                      setSlashSelection((current) => (current + direction + slashSuggestions.length) % slashSuggestions.length);
                      return;
                    }
                    if (slashSuggestions.length && event.key === "Tab" && !event.shiftKey) {
                      event.preventDefault();
                      selectSlashSuggestion(slashSuggestions[slashSelection]);
                      return;
                    }
                    if (slashSuggestions.length && event.key === "Enter" && !event.shiftKey) {
                      const selected = slashSuggestions[slashSelection];
                      const exact = prompt.trim() === `/${selected.name}`;
                      if (!exact || prompt.trim() === "/skills") {
                        event.preventDefault();
                        selectSlashSuggestion(selected);
                        return;
                      }
                    }
                    if (event.key === "Enter" && !event.shiftKey) {
                      event.preventDefault();
                      void submit();
                    }
                  }}
                  placeholder={runtimeId ? busy ? "Add guidance for the next turn…" : repo ? "Describe a coding task…" : "Ask Medusa anything…" : "Starting Medusa…"}
                  rows={3}
                />
                <div className="composer-bottom">
                  <div className="composer-tools">
                    <button onClick={addFiles} disabled={!runtimeId} title="Attach repository files"><FilePlus2 size={16} /></button>
                    <span>Shift+Enter for a new line</span>
                  </div>
                  <div className="composer-actions">
                    {busy && <button className="cancel-button" onClick={cancel}><Square size={13} /> Cancel</button>}
                    <button className="send-button" onClick={submit} disabled={!runtimeId || (!prompt.trim() && attachments.length === 0)}><Send size={16} /></button>
                  </div>
                </div>
              </div>
            </footer>
          </>
        )}

        {activePanel === "plan" && (
          <div className="standalone-panel">
            <div className="panel-title"><ListChecks size={18} /><div><h2>Execution plan</h2><p>Live plan state from medusa-runtime</p></div></div>
            {plan.length ? plan.map((step) => (
              <div className={`plan-row ${step.status}`} key={step.title}>{planIcon(step.status)}<span>{step.title}</span></div>
            )) : <p className="muted-copy">No plan has been created for this session.</p>}
            <button className="secondary-action" disabled={!runtimeId} onClick={() => void sendText("/plan", [])}>Enter plan mode</button>
          </div>
        )}

        {activePanel === "settings" && (
          <div className="standalone-panel settings-form">
            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Saved securely in your operating system credential manager</p></div></div>
            <label>Provider<select value={provider} onChange={(event) => setProvider(event.target.value)}><option value="minimax">MiniMax</option><option value="anthropic">Anthropic</option><option value="anthropic-compatible">Anthropic-compatible</option></select></label>
            <label>Model<input value={model} onChange={(event) => setModel(event.target.value)} /></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}><option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
            <label>API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="Leave blank to use the saved key" /></label>
            <button className="primary-action" onClick={applyModel} disabled={!runtimeId}>Apply configuration</button>
          </div>
        )}
      </section>

      <aside className="inspector">
        <section className="inspector-section">
          <div className="panel-heading"><span><Gauge size={15} /> Session</span></div>
          <dl className="metric-grid">
            <div><dt>Model</dt><dd>{settings.model}</dd></div>
            <div><dt>Effort</dt><dd>{settings.effort.replace("effort:", "")}</dd></div>
            <div><dt>Mode</dt><dd>{settings.planMode ? "Plan" : "Full"}</dd></div>
            <div><dt>Credential</dt><dd>{settings.credentialConfigured ? "Ready" : "Missing"}</dd></div>
          </dl>
        </section>
        <section className="inspector-section">
          <div className="panel-heading"><span><Activity size={15} /> Usage</span></div>
          <dl className="metric-grid tokens">
            <div><dt>Input</dt><dd>{usage.input.toLocaleString()}</dd></div>
            <div><dt>Output</dt><dd>{usage.output.toLocaleString()}</dd></div>
            <div><dt>Cached</dt><dd>{usage.cached.toLocaleString()}</dd></div>
            <div><dt>Total</dt><dd>{totalTokens.toLocaleString()}</dd></div>
          </dl>
          <p className="metric-footnote">Model time: {(usage.elapsed / 1000).toFixed(1)}s</p>
        </section>
        <section className="inspector-section inspector-grow">
          <div className="panel-heading"><span><ListChecks size={15} /> Plan</span><small>{plan.filter((step) => step.status === "completed").length}/{plan.length}</small></div>
          <div className="mini-plan">
            {plan.length ? plan.map((step) => <div key={step.title} className={step.status}>{planIcon(step.status)}<span>{step.title}</span></div>) : <p>No active plan</p>}
          </div>
        </section>
        <section className="inspector-section inspector-grow">
          <div className="panel-heading"><span><Activity size={15} /> Activity</span><small>{activities.length}</small></div>
          <div className="activity-list">
            {activities.length ? activities.slice(-12).reverse().map((item, index) => (
              <div key={item.id ?? `${item.title}-${index}`} className={item.kind}><span>{item.kind === "error" ? <OctagonX size={14} /> : item.kind === "done" ? <CheckCircle2 size={14} /> : <Activity size={14} />}</span><div><strong>{item.title}</strong>{item.details.map((detail) => <small key={detail}>{detail}</small>)}</div></div>
            )) : <p>No tool activity yet</p>}
          </div>
        </section>
      </aside>
    </main>
  );
}
