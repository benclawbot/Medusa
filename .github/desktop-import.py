from __future__ import annotations

import shutil
import sys
from pathlib import Path

root = Path.cwd()
zeus = Path(sys.argv[1]).resolve()
app = root / "apps/medusa-desktop"
src = app / "src"
tauri = app / "src-tauri"

for directory in [src / "test", tauri / "src", tauri / "capabilities", tauri / "icons"]:
    directory.mkdir(parents=True, exist_ok=True)


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content.strip() + "\n")


write(
    app / "package.json",
    r'''
{
  "name": "medusa-desktop",
  "version": "1.0.0",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "typecheck": "tsc --noEmit",
    "test": "vitest run",
    "tauri": "tauri",
    "tauri:dev": "tauri dev",
    "tauri:build": "tauri build"
  },
  "dependencies": {
    "@tauri-apps/api": "^2.0.0",
    "@tauri-apps/plugin-dialog": "^2.7.1",
    "lucide-react": "^0.468.0",
    "react": "^18.3.1",
    "react-dom": "^18.3.1"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2.0.0",
    "@testing-library/jest-dom": "^6.6.3",
    "@testing-library/react": "^16.1.0",
    "@testing-library/user-event": "^14.5.2",
    "@types/node": "^26.1.0",
    "@types/react": "^18.3.18",
    "@types/react-dom": "^18.3.5",
    "@vitejs/plugin-react": "^4.3.4",
    "jsdom": "^25.0.1",
    "typescript": "^5.7.2",
    "vite": "^6.0.7",
    "vitest": "^4.1.9"
  }
}
''',
)

write(
    app / "tsconfig.json",
    r'''
{
  "compilerOptions": {
    "target": "ES2020",
    "useDefineForClassFields": true,
    "lib": ["DOM", "DOM.Iterable", "ES2020"],
    "allowJs": false,
    "skipLibCheck": true,
    "esModuleInterop": true,
    "allowSyntheticDefaultImports": true,
    "strict": true,
    "forceConsistentCasingInFileNames": true,
    "module": "ESNext",
    "moduleResolution": "Node",
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "types": ["vitest/globals", "@testing-library/jest-dom", "node"]
  },
  "include": ["src"],
  "references": [{ "path": "./tsconfig.node.json" }]
}
''',
)

write(
    app / "tsconfig.node.json",
    r'''
{
  "compilerOptions": {
    "composite": true,
    "skipLibCheck": true,
    "module": "ESNext",
    "moduleResolution": "Node",
    "allowSyntheticDefaultImports": true
  },
  "include": ["vite.config.ts"]
}
''',
)

write(
    app / "vite.config.ts",
    r'''
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    strictPort: true,
    port: 5173,
  },
  envPrefix: ["VITE_", "TAURI_"],
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    exclude: [...configDefaults.exclude, "src-tauri/**"],
  },
});
''',
)

write(
    app / "index.html",
    r'''
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <meta name="theme-color" content="#f6f7fb" />
    <title>Medusa Desktop</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
''',
)

write(
    src / "main.tsx",
    r'''
import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import "./styles.css";
import "./medusa-desktop.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
''',
)

write(
    src / "runtime.ts",
    r'''
import { invoke } from "@tauri-apps/api/core";

export type Effort = "low" | "medium" | "high" | "auto";
export type SubmitDisposition = "started" | "queued";

export interface RuntimeStartResponse {
  runtimeId: string;
  repo: string;
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

export interface ModelConfiguration {
  provider: string;
  model: string;
  effort: Effort;
  apiKey?: string;
}

export async function startRuntime(repo: string): Promise<RuntimeStartResponse> {
  return invoke<RuntimeStartResponse>("runtime_start", { repo });
}

export async function closeRuntime(runtimeId: string): Promise<void> {
  await invoke("runtime_close", { runtimeId });
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

export async function cancelRuntime(runtimeId: string): Promise<boolean> {
  return invoke<boolean>("runtime_cancel", { runtimeId });
}

export async function pollRuntime(runtimeId: string): Promise<RuntimeEvent[]> {
  return invoke<RuntimeEvent[]>("runtime_poll", { runtimeId, maxEvents: 200 });
}

export async function configureRuntime(
  runtimeId: string,
  configuration: ModelConfiguration,
): Promise<void> {
  await invoke("runtime_configure_model", { runtimeId, configuration });
}
''',
)

write(
    src / "App.tsx",
    r'''
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
import {
  cancelRuntime,
  closeRuntime,
  configureRuntime,
  pollRuntime,
  runRuntimeCommand,
  startRuntime,
  submitRuntime,
  type DesktopAttachment,
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

const emptyUsage: UsageState = { input: 0, output: 0, cached: 0, cacheWrite: 0, elapsed: 0 };
let messageCounter = 0;
const nextMessageId = () => ++messageCounter;

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
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
  const [attachments, setAttachments] = useState<DesktopAttachment[]>([]);
  const [busy, setBusy] = useState(false);
  const [turn, setTurn] = useState(0);
  const [error, setError] = useState<string>();
  const [provider, setProvider] = useState("minimax");
  const [model, setModel] = useState("MiniMax-M2.5");
  const [effort, setEffort] = useState<Effort>("auto");
  const [apiKey, setApiKey] = useState("");
  const [activePanel, setActivePanel] = useState<"chat" | "plan" | "settings">("chat");
  const pollBusy = useRef(false);
  const transcriptRef = useRef<HTMLDivElement>(null);

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
    transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight, behavior: "smooth" });
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
    const previous = window.localStorage.getItem("medusa.desktop.repo");
    if (!previous) return;
    void startRuntime(previous)
      .then((started) => {
        setRuntimeId(started.runtimeId);
        setRepo(started.repo);
      })
      .catch(() => window.localStorage.removeItem("medusa.desktop.repo"));
  }, []);

  useEffect(() => () => {
    if (runtimeId) void closeRuntime(runtimeId);
  }, [runtimeId]);

  const openProject = async () => {
    const selected = await open({ directory: true, multiple: false, title: "Open a Medusa project" });
    if (typeof selected !== "string") return;
    try {
      if (runtimeId) await closeRuntime(runtimeId);
      const started = await startRuntime(selected);
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

  const applyModel = async () => {
    if (!runtimeId) return;
    try {
      await configureRuntime(runtimeId, {
        provider,
        model,
        effort,
        apiKey: apiKey.trim() || undefined,
      });
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

  const repoName = useMemo(() => basename(repo) || "No project", [repo]);
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
          <p className="section-label">Project</p>
          <button className="project-picker" onClick={openProject}>
            <FolderOpen size={16} />
            <span><strong>{repoName}</strong><small>{repo || "Choose a repository"}</small></span>
            <ChevronRight size={15} />
          </button>
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
            {busy ? `Working · turn ${turn}` : runtimeId ? "Ready" : "Open a project"}
          </div>
        </header>

        {activePanel === "chat" && (
          <>
            <div className="transcript" ref={transcriptRef}>
              {!runtimeId && (
                <div className="empty-state">
                  <span className="empty-icon"><Bot size={28} /></span>
                  <h2>Open a project to start Medusa</h2>
                  <p>The desktop interface uses the same runtime, tools, memory, safety policy, and sessions as the terminal interface.</p>
                  <button className="primary-action" onClick={openProject}><FolderOpen size={16} /> Open project</button>
                </div>
              )}
              {runtimeId && messages.length === 0 && (
                <div className="empty-state compact">
                  <h2>What should Medusa build?</h2>
                  <p>Describe a coding task, paste a screenshot, attach repository files, or use a slash command.</p>
                </div>
              )}
              {messages.map((message) => (
                <article className={`message ${message.role}`} key={message.id}>
                  <div className="message-heading">
                    <span>{message.role === "user" ? "You" : message.role === "assistant" ? "Medusa" : "Runtime"}</span>
                    {message.queued && <small>queued for next turn</small>}
                  </div>
                  <div className="message-body">{message.text}</div>
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
              {!!questions.length && (
                <section className="question-card">
                  {questions.map((question) => (
                    <div key={`${question.header}-${question.question}`}>
                      <small>{question.header}</small>
                      <strong>{question.question}</strong>
                      <div className="question-options">
                        {question.options.map((option) => (
                          <button key={option.label} onClick={() => void sendText(option.label, [])}>
                            <span>{option.label}</span><small>{option.description}</small>
                          </button>
                        ))}
                      </div>
                    </div>
                  ))}
                </section>
              )}
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
                <textarea
                  value={prompt}
                  disabled={!runtimeId}
                  onChange={(event) => setPrompt(event.target.value)}
                  onPaste={onPaste}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !event.shiftKey) {
                      event.preventDefault();
                      void submit();
                    }
                  }}
                  placeholder={runtimeId ? busy ? "Add guidance for the next turn…" : "Describe a coding task…" : "Open a project first"}
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
            <div className="panel-title"><Settings size={18} /><div><h2>Model settings</h2><p>Session-only configuration</p></div></div>
            <label>Provider<select value={provider} onChange={(event) => setProvider(event.target.value)}><option value="minimax">MiniMax</option><option value="anthropic">Anthropic</option><option value="anthropic-compatible">Anthropic-compatible</option></select></label>
            <label>Model<input value={model} onChange={(event) => setModel(event.target.value)} /></label>
            <label>Effort<select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}><option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
            <label>Session API key<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="Stored only for this runtime session" /></label>
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
''',
)

# Retain Zeus's visual system as the design source, then layer only the classes used by the new runtime-driven shell.
shutil.copyfile(zeus / "src/styles.css", src / "styles.css")
write(
    src / "medusa-desktop.css",
    r'''
.medusa-shell { --purple: #6847e8; }
.medusa-shell button:disabled { cursor: not-allowed; opacity: .48; }
.medusa-shell .brand-row > div { min-width: 0; }
.medusa-shell .brand-row small { color: var(--muted); }
.medusa-shell .brand-row .version { margin-left: auto; }
.medusa-shell .new-session span { display: flex; align-items: center; gap: 8px; }
.sidebar-spacer { flex: 1; }
.project-card { border-top: 1px solid var(--line); padding-top: 16px; }
.project-picker { display: grid; grid-template-columns: auto minmax(0,1fr) auto; align-items: center; gap: 10px; width: 100%; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); padding: 10px; text-align: left; color: var(--text); }
.project-picker span { display: grid; min-width: 0; }
.project-picker strong, .project-picker small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.project-picker small { color: var(--muted); font-size: .72rem; }
.security-note { display: flex; gap: 8px; align-items: center; color: var(--muted); font-size: .76rem; border-top: 1px solid var(--line); padding-top: 14px; }
.medusa-workspace { min-width: 0; display: flex; flex-direction: column; background: var(--surface-soft); }
.medusa-workspace .topbar { justify-content: space-between; min-height: 72px; padding: 0 24px; border-bottom: 1px solid var(--line); background: rgba(255,255,255,.82); }
.eyebrow { margin: 0 0 2px; color: var(--muted); font-size: .72rem; text-transform: uppercase; letter-spacing: .05em; }
.runtime-state { display: flex; align-items: center; gap: 8px; color: var(--muted); font-size: .8rem; }
.status-dot { width: 8px; height: 8px; border-radius: 50%; background: #a9afbd; }
.status-dot.ready { background: var(--green); }.status-dot.busy { background: var(--purple); box-shadow: 0 0 0 4px var(--purple-soft); }.status-dot.offline { background: #a9afbd; }
.transcript { flex: 1; min-height: 0; overflow-y: auto; padding: 24px clamp(20px,5vw,72px); }
.empty-state { min-height: 100%; display: grid; place-content: center; justify-items: center; text-align: center; color: var(--muted); }
.empty-state.compact { min-height: 48%; }.empty-state h2 { color: var(--text); margin: 14px 0 8px; }.empty-state p { max-width: 560px; margin: 0 0 20px; }
.empty-icon { display: grid; place-items: center; width: 58px; height: 58px; border-radius: 16px; color: var(--purple); background: var(--purple-soft); }
.primary-action, .secondary-action { display: inline-flex; align-items: center; justify-content: center; gap: 8px; border-radius: 8px; padding: 9px 14px; }
.primary-action { border: 1px solid var(--purple); color: white; background: var(--purple); }.secondary-action { border: 1px solid var(--line-strong); color: var(--text); background: var(--surface); }
.message { max-width: 850px; margin: 0 auto 18px; border-radius: 12px; padding: 14px 16px; }
.message.user { background: #eef0f5; margin-left: auto; max-width: 72%; }.message.assistant { background: var(--surface); border: 1px solid var(--line); }.message.system { border-left: 3px solid var(--purple); background: var(--purple-soft); color: #4b3a93; }
.message-heading { justify-content: space-between; margin-bottom: 7px; color: var(--muted); font-size: .75rem; font-weight: 650; }.message-body { white-space: pre-wrap; overflow-wrap: anywhere; }
.message-attachments, .attachment-strip { display: flex; flex-wrap: wrap; gap: 7px; margin-top: 10px; }.message-attachments span, .attachment-strip > span { display: inline-flex; align-items: center; gap: 5px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); padding: 5px 8px; font-size: .74rem; }
.attachment-strip > span button { display: grid; place-items: center; border: 0; background: transparent; color: var(--muted); padding: 0; }
.question-card { max-width: 850px; margin: 0 auto 18px; border: 1px solid #dcd5ff; border-radius: 12px; background: var(--purple-soft); padding: 16px; }.question-card > div { display: grid; gap: 7px; }.question-card small { color: var(--muted); }.question-options { display: grid; gap: 8px; margin-top: 5px; }.question-options button { display: grid; gap: 2px; text-align: left; border: 1px solid var(--line); border-radius: 8px; background: white; padding: 9px 11px; }.question-options button span { font-weight: 650; }
.thinking-row { display: flex; gap: 8px; align-items: center; max-width: 850px; margin: 0 auto; color: var(--muted); }
.composer-wrap { padding: 12px clamp(20px,5vw,72px) 22px; background: linear-gradient(transparent, var(--surface-soft) 18%); }.composer-card { max-width: 850px; margin: 0 auto; border: 1px solid var(--line-strong); border-radius: 12px; background: white; box-shadow: 0 12px 32px rgba(20,27,45,.08); }.composer-card textarea { width: 100%; resize: none; border: 0; outline: 0; background: transparent; padding: 14px 16px 8px; color: var(--text); }.composer-bottom { justify-content: space-between; padding: 7px 10px 10px; }.composer-tools, .composer-actions { display: flex; align-items: center; gap: 8px; }.composer-tools button, .send-button, .cancel-button { display: inline-flex; align-items: center; justify-content: center; gap: 6px; border-radius: 7px; }.composer-tools span { color: var(--muted); font-size: .7rem; }.send-button { width: 34px; height: 31px; color: white; background: var(--purple); }.cancel-button { border: 1px solid var(--line); background: white; padding: 6px 9px; color: var(--danger); }
.error-banner { display: flex; gap: 8px; align-items: center; max-width: 850px; margin: 0 auto 8px; border: 1px solid #fecaca; border-radius: 8px; background: #fff1f2; color: #b42318; padding: 8px 10px; font-size: .78rem; }
.standalone-panel { flex: 1; overflow: auto; padding: 32px clamp(24px,7vw,100px); }.panel-title { display: flex; gap: 12px; align-items: flex-start; margin-bottom: 24px; }.panel-title h2, .panel-title p { margin: 0; }.panel-title p { color: var(--muted); }.plan-row { display: flex; gap: 10px; align-items: center; border-bottom: 1px solid var(--line); padding: 12px 4px; }.plan-row.completed { color: var(--green); }.plan-row.failed { color: var(--danger); }.plan-row.inProgress { color: var(--purple); }.muted-copy { color: var(--muted); }
.settings-form { display: grid; align-content: start; gap: 16px; }.settings-form .panel-title { margin-bottom: 4px; }.settings-form label { display: grid; gap: 6px; max-width: 620px; font-size: .78rem; color: var(--muted); }.settings-form input, .settings-form select { border: 1px solid var(--line-strong); border-radius: 8px; background: white; padding: 9px 10px; color: var(--text); }.settings-form .primary-action { width: max-content; }
.inspector-section { border-bottom: 1px solid var(--line); padding: 18px 16px; }.inspector-grow { min-height: 0; }.panel-heading { justify-content: space-between; margin-bottom: 13px; color: var(--text); font-weight: 650; }.panel-heading span { display: flex; align-items: center; gap: 7px; }.panel-heading small { color: var(--muted); }
.metric-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin: 0; }.metric-grid div { min-width: 0; }.metric-grid dt { color: var(--muted); font-size: .69rem; text-transform: uppercase; }.metric-grid dd { margin: 2px 0 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }.metric-grid.tokens dd { font-variant-numeric: tabular-nums; }.metric-footnote { margin: 9px 0 0; color: var(--muted); font-size: .7rem; }
.mini-plan, .activity-list { display: grid; gap: 8px; }.mini-plan > div { display: grid; grid-template-columns: auto 1fr; gap: 8px; align-items: start; color: var(--muted); font-size: .76rem; }.mini-plan .completed { color: var(--green); }.mini-plan .failed { color: var(--danger); }.mini-plan .inProgress { color: var(--purple); }.mini-plan p, .activity-list p { color: var(--muted); font-size: .76rem; }
.activity-list > div { display: grid; grid-template-columns: auto 1fr; gap: 8px; align-items: start; border: 1px solid var(--line); border-radius: 8px; padding: 8px; }.activity-list strong, .activity-list small { display: block; }.activity-list strong { font-size: .76rem; }.activity-list small { color: var(--muted); font-size: .69rem; }.activity-list .error { border-color: #fecaca; color: var(--danger); }.activity-list .done { color: var(--green); }
@media (max-width: 1120px) { .app-shell { grid-template-columns: 220px minmax(480px,1fr); }.inspector { display: none; } }
''',
)

write(src / "test/setup.ts", 'import "@testing-library/jest-dom";')
write(
    src / "runtime.test.ts",
    r'''
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { pollRuntime, startRuntime, submitRuntime } from "./runtime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);

describe("desktop runtime adapter", () => {
  beforeEach(() => mockedInvoke.mockReset());

  it("starts the shared runtime for a selected repository", async () => {
    mockedInvoke.mockResolvedValueOnce({ runtimeId: "runtime-1", repo: "/repo" });
    await expect(startRuntime("/repo")).resolves.toEqual({ runtimeId: "runtime-1", repo: "/repo" });
    expect(mockedInvoke).toHaveBeenCalledWith("runtime_start", { repo: "/repo" });
  });

  it("submits prompts and polls typed events", async () => {
    mockedInvoke.mockResolvedValueOnce("queued");
    await expect(submitRuntime("runtime-1", { text: "more detail", attachments: [], revision: 2 })).resolves.toBe("queued");
    mockedInvoke.mockResolvedValueOnce([{ type: "progress", turn: 4 }]);
    await expect(pollRuntime("runtime-1")).resolves.toEqual([{ type: "progress", turn: 4 }]);
  });
});
''',
)

write(
    src / "App.test.tsx",
    r'''
import { render, screen } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./runtime", async () => {
  const actual = await vi.importActual<typeof import("./runtime")>("./runtime");
  return {
    ...actual,
    startRuntime: vi.fn(),
    closeRuntime: vi.fn(),
    pollRuntime: vi.fn().mockResolvedValue([]),
    submitRuntime: vi.fn(),
    runRuntimeCommand: vi.fn(),
    cancelRuntime: vi.fn(),
    configureRuntime: vi.fn(),
  };
});

beforeEach(() => window.localStorage.clear());

it("presents the Zeus-derived shell without claiming a separate agent backend", () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Medusa" })).toBeInTheDocument();
  expect(screen.getByText("Open a project to start Medusa")).toBeInTheDocument();
  expect(screen.getByText("Medusa policy remains authoritative")).toBeInTheDocument();
});
''',
)

write(
    tauri / "Cargo.toml",
    r'''
[package]
name = "medusa-desktop"
version = "1.0.0"
description = "Zeus-derived desktop interface for the Medusa runtime"
authors = ["benclawbot"]
edition = "2024"
rust-version = "1.88"
license = "MIT"
repository = "https://github.com/benclawbot/Medusa"

[lib]
name = "medusa_desktop_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
base64 = "0.22"
image = { version = "0.25", default-features = false, features = ["gif", "jpeg", "png", "webp"] }
medusa-runtime = { path = "../../../crates/medusa-runtime" }
serde = { version = "1", features = ["derive"] }
tauri = { version = "2", features = [] }
tauri-plugin-dialog = "2"

[workspace]
''',
)

write(tauri / "build.rs", "fn main() { tauri_build::build(); }")
write(
    tauri / "src/main.rs",
    r'''
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    medusa_desktop_lib::run();
}
''',
)

write(
    tauri / "src/dto.rs",
    r'''
use medusa_runtime::{AgentPlanStepStatus, RuntimeActivityKind, RuntimeEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStartResponse {
    pub runtime_id: String,
    pub repo: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPromptDraft {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<DesktopAttachment>,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DesktopAttachment {
    File { path: String },
    Image { name: String, data_url: String },
    Text { name: String, text: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopModelConfiguration {
    pub provider: String,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopSubmitDisposition {
    Started,
    Queued,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DesktopRuntimeEvent {
    Started,
    AssistantText {
        text: String,
    },
    Activity {
        activity: DesktopActivity,
    },
    Plan {
        steps: Vec<DesktopPlanStep>,
    },
    Question {
        prompts: Vec<DesktopQuestionPrompt>,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        model_elapsed_millis: u64,
    },
    Progress {
        turn: u32,
    },
    Settings {
        model: String,
        effort: String,
        plan_mode: bool,
        credential_configured: bool,
    },
    Notice {
        title: String,
        details: Vec<String>,
    },
    NewSession,
    Compacted {
        message: String,
    },
    Completed {
        session_id: String,
    },
    TurnFinished,
    Cancelled,
    Failed {
        message: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopActivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: DesktopActivityKind,
    pub title: String,
    pub details: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopActivityKind {
    Assistant,
    Done,
    Error,
    Tool,
    Verification,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPlanStep {
    pub title: String,
    pub status: DesktopPlanStepStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopPlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopQuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<DesktopQuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Serialize)]
pub struct DesktopQuestionOption {
    pub label: String,
    pub description: String,
}

impl From<RuntimeEvent> for DesktopRuntimeEvent {
    fn from(event: RuntimeEvent) -> Self {
        match event {
            RuntimeEvent::Started => Self::Started,
            RuntimeEvent::AssistantText(text) => Self::AssistantText { text },
            RuntimeEvent::Activity(activity) => Self::Activity {
                activity: DesktopActivity {
                    id: activity.id,
                    kind: match activity.kind {
                        RuntimeActivityKind::Assistant => DesktopActivityKind::Assistant,
                        RuntimeActivityKind::Done => DesktopActivityKind::Done,
                        RuntimeActivityKind::Error => DesktopActivityKind::Error,
                        RuntimeActivityKind::Tool => DesktopActivityKind::Tool,
                        RuntimeActivityKind::Verification => DesktopActivityKind::Verification,
                    },
                    title: activity.title,
                    details: activity.details,
                },
            },
            RuntimeEvent::Plan(steps) => Self::Plan {
                steps: steps
                    .into_iter()
                    .map(|step| DesktopPlanStep {
                        title: step.title,
                        status: match step.status {
                            AgentPlanStepStatus::Pending => DesktopPlanStepStatus::Pending,
                            AgentPlanStepStatus::InProgress => DesktopPlanStepStatus::InProgress,
                            AgentPlanStepStatus::Completed => DesktopPlanStepStatus::Completed,
                            AgentPlanStepStatus::Failed => DesktopPlanStepStatus::Failed,
                        },
                    })
                    .collect(),
            },
            RuntimeEvent::Question(question) => Self::Question {
                prompts: question
                    .prompts()
                    .iter()
                    .map(|prompt| DesktopQuestionPrompt {
                        header: prompt.header.clone(),
                        question: prompt.question.clone(),
                        options: prompt
                            .options
                            .iter()
                            .map(|option| DesktopQuestionOption {
                                label: option.label.clone(),
                                description: option.description.clone(),
                            })
                            .collect(),
                        multi_select: prompt.multi_select,
                    })
                    .collect(),
            },
            RuntimeEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            } => Self::Usage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                model_elapsed_millis,
            },
            RuntimeEvent::Progress { turn } => Self::Progress { turn },
            RuntimeEvent::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            } => Self::Settings {
                model,
                effort,
                plan_mode,
                credential_configured,
            },
            RuntimeEvent::Notice { title, details } => Self::Notice { title, details },
            RuntimeEvent::NewSession => Self::NewSession,
            RuntimeEvent::Compacted { message } => Self::Compacted { message },
            RuntimeEvent::Completed { session_id } => Self::Completed { session_id },
            RuntimeEvent::TurnFinished => Self::TurnFinished,
            RuntimeEvent::Cancelled => Self::Cancelled,
            RuntimeEvent::Failed(message) => Self::Failed { message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_runtime::{RuntimeActivity, RuntimePlanStep};

    #[test]
    fn maps_plan_and_activity_events_without_tui_types() {
        let plan = DesktopRuntimeEvent::from(RuntimeEvent::Plan(vec![RuntimePlanStep {
            title: "Wire desktop".to_owned(),
            status: AgentPlanStepStatus::InProgress,
        }]));
        assert!(matches!(plan, DesktopRuntimeEvent::Plan { steps } if matches!(steps[0].status, DesktopPlanStepStatus::InProgress)));

        let activity = DesktopRuntimeEvent::from(RuntimeEvent::Activity(RuntimeActivity {
            id: Some("tool-1".to_owned()),
            kind: RuntimeActivityKind::Tool,
            title: "Read file".to_owned(),
            details: Vec::new(),
        }));
        assert!(matches!(activity, DesktopRuntimeEvent::Activity { activity } if activity.id.as_deref() == Some("tool-1")));
    }
}
''',
)

write(
    tauri / "src/runtime.rs",
    r'''
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use medusa_runtime::{
    RuntimeController, SubmitDisposition,
    commands::{Effort, ModelConfiguration, parse_slash_command},
    prompt::{
        ClipboardImage, FileAttachment, PromptAttachment, PromptDraft, TextAttachment,
        MAX_CLIPBOARD_TEXT_BYTES, MAX_TOTAL_ATTACHMENT_BYTES,
    },
};
use tauri::State;

use crate::dto::{
    DesktopAttachment, DesktopModelConfiguration, DesktopPromptDraft, DesktopRuntimeEvent,
    DesktopSubmitDisposition, RuntimeStartResponse,
};

struct RuntimeEntry {
    repo: PathBuf,
    controller: RuntimeController,
}

#[derive(Default)]
pub struct RuntimeRegistry {
    next_id: AtomicU64,
    entries: Mutex<BTreeMap<String, RuntimeEntry>>,
}

impl RuntimeRegistry {
    fn insert(&self, repo: PathBuf) -> Result<RuntimeStartResponse, String> {
        let id = format!("desktop-runtime-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let controller = RuntimeController::start(repo.clone());
        self.entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
            .insert(id.clone(), RuntimeEntry { repo: repo.clone(), controller });
        Ok(RuntimeStartResponse {
            runtime_id: id,
            repo: repo.to_string_lossy().into_owned(),
        })
    }

    fn with_entry<T>(
        &self,
        runtime_id: &str,
        action: impl FnOnce(&RuntimeEntry) -> Result<T, String>,
    ) -> Result<T, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "desktop runtime registry is poisoned".to_owned())?;
        let entry = entries
            .get(runtime_id)
            .ok_or_else(|| format!("runtime {runtime_id} does not exist"))?;
        action(entry)
    }
}

#[tauri::command]
pub fn runtime_start(
    repo: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<RuntimeStartResponse, String> {
    let repo = canonical_directory(Path::new(&repo))?;
    registry.insert(repo)
}

#[tauri::command]
pub fn runtime_close(
    runtime_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    registry
        .entries
        .lock()
        .map_err(|_| "desktop runtime registry is poisoned".to_owned())?
        .remove(&runtime_id)
        .ok_or_else(|| format!("runtime {runtime_id} does not exist"))?;
    Ok(())
}

#[tauri::command]
pub fn runtime_submit(
    runtime_id: String,
    draft: DesktopPromptDraft,
    registry: State<'_, RuntimeRegistry>,
) -> Result<DesktopSubmitDisposition, String> {
    registry.with_entry(&runtime_id, |entry| {
        let draft = convert_prompt(&entry.repo, draft)?;
        entry
            .controller
            .submit(draft)
            .map(|disposition| match disposition {
                SubmitDisposition::Started => DesktopSubmitDisposition::Started,
                SubmitDisposition::Queued => DesktopSubmitDisposition::Queued,
            })
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn runtime_command(
    runtime_id: String,
    input: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let command = parse_slash_command(&input)
        .map_err(|error| format!("invalid slash command: {error}"))?
        .ok_or_else(|| "runtime_command expects a slash command".to_owned())?;
    registry.with_entry(&runtime_id, |entry| {
        entry.controller.run_command(command).map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn runtime_cancel(
    runtime_id: String,
    registry: State<'_, RuntimeRegistry>,
) -> Result<bool, String> {
    registry.with_entry(&runtime_id, |entry| Ok(entry.controller.cancel()))
}

#[tauri::command]
pub fn runtime_poll(
    runtime_id: String,
    max_events: Option<usize>,
    registry: State<'_, RuntimeRegistry>,
) -> Result<Vec<DesktopRuntimeEvent>, String> {
    registry.with_entry(&runtime_id, |entry| {
        let mut events = Vec::new();
        let limit = max_events.unwrap_or(200).clamp(1, 500);
        while events.len() < limit {
            match entry.controller.try_event().map_err(|error| error.to_string())? {
                Some(event) => events.push(event.into()),
                None => break,
            }
        }
        Ok(events)
    })
}

#[tauri::command]
pub fn runtime_configure_model(
    runtime_id: String,
    configuration: DesktopModelConfiguration,
    registry: State<'_, RuntimeRegistry>,
) -> Result<(), String> {
    let effort = match configuration.effort.to_ascii_lowercase().as_str() {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "auto" => Effort::Auto,
        _ => return Err("effort must be low, medium, high, or auto".to_owned()),
    };
    registry.with_entry(&runtime_id, |entry| {
        entry
            .controller
            .configure_model(ModelConfiguration {
                provider: configuration.provider,
                model: configuration.model,
                effort,
                api_key: configuration.api_key.filter(|key| !key.trim().is_empty()),
            })
            .map_err(|error| error.to_string())
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical)
}

fn convert_prompt(repo: &Path, source: DesktopPromptDraft) -> Result<PromptDraft, String> {
    let mut draft = PromptDraft {
        text: source.text,
        attachments: Vec::new(),
        revision: source.revision,
    };
    for attachment in source.attachments {
        match attachment {
            DesktopAttachment::File { path } => attach_file(repo, &mut draft, Path::new(&path))?,
            DesktopAttachment::Image { name, data_url } => {
                attach_image(&mut draft, &name, &data_url)?;
            }
            DesktopAttachment::Text { name, text } => attach_text(&mut draft, name, text)?,
        }
    }
    Ok(draft)
}

fn attach_file(repo: &Path, draft: &mut PromptDraft, path: &Path) -> Result<(), String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot attach {}: {error}", path.display()))?;
    if !canonical.starts_with(repo) {
        return Err(format!(
            "attachment {} is outside the selected repository",
            canonical.display()
        ));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect {}: {error}", canonical.display()))?;
    if !metadata.is_file() {
        return Err(format!("attachment {} is not a file", canonical.display()));
    }
    let byte_len = usize::try_from(metadata.len())
        .map_err(|_| format!("attachment {} is too large", canonical.display()))?;
    ensure_total(draft, byte_len)?;
    draft.attachments.push(PromptAttachment::File(FileAttachment {
        path: canonical,
        byte_len,
    }));
    Ok(())
}

fn attach_text(draft: &mut PromptDraft, name: String, text: String) -> Result<(), String> {
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return Err(format!("text attachment {name} exceeds the clipboard text limit"));
    }
    ensure_total(draft, text.len())?;
    draft
        .attachments
        .push(PromptAttachment::PastedText(TextAttachment {
            display_name: name,
            text,
        }));
    Ok(())
}

fn attach_image(draft: &mut PromptDraft, name: &str, data_url: &str) -> Result<(), String> {
    let (header, encoded) = data_url
        .split_once(',')
        .ok_or_else(|| format!("image attachment {name} is not a data URL"))?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err(format!("image attachment {name} must be a base64 image data URL"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
    let image = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("cannot detect image attachment {name}: {error}"))?
        .decode()
        .map_err(|error| format!("cannot decode image attachment {name}: {error}"))?;
    let rgba = image.to_rgba8();
    draft
        .add_image(ClipboardImage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            source_format: Some(header.trim_start_matches("data:").trim_end_matches(";base64").to_owned()),
        })
        .map_err(|error| error.to_string())?;
    if let Some(PromptAttachment::Image(image)) = draft.attachments.last_mut() {
        image.display_name = name.to_owned();
    }
    Ok(())
}

fn ensure_total(draft: &PromptDraft, additional: usize) -> Result<(), String> {
    let total = draft.total_attachment_bytes().saturating_add(additional);
    if total > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(format!(
            "prompt attachments total {total} bytes; limit is {MAX_TOTAL_ATTACHMENT_BYTES}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_attachments_are_confined_to_the_selected_repository() {
        let repo = tempdir().expect("repo");
        let outside = tempdir().expect("outside");
        let path = outside.path().join("secret.txt");
        fs::write(&path, "secret").expect("write outside file");
        let error = convert_prompt(
            repo.path(),
            DesktopPromptDraft {
                text: String::new(),
                attachments: vec![DesktopAttachment::File {
                    path: path.to_string_lossy().into_owned(),
                }],
                revision: 0,
            },
        )
        .expect_err("outside attachment must fail");
        assert!(error.contains("outside the selected repository"));
    }

    #[test]
    fn repository_file_attachment_keeps_canonical_path_and_size() {
        let repo = tempdir().expect("repo");
        let path = repo.path().join("context.txt");
        fs::write(&path, "context").expect("write file");
        let draft = convert_prompt(
            repo.path(),
            DesktopPromptDraft {
                text: "review this".to_owned(),
                attachments: vec![DesktopAttachment::File {
                    path: path.to_string_lossy().into_owned(),
                }],
                revision: 4,
            },
        )
        .expect("valid attachment");
        assert_eq!(draft.revision, 4);
        assert!(matches!(&draft.attachments[0], PromptAttachment::File(file) if file.byte_len == 7));
    }
}
''',
)

write(
    tauri / "src/lib.rs",
    r'''
mod dto;
mod runtime;

use runtime::{
    RuntimeRegistry, runtime_cancel, runtime_close, runtime_command, runtime_configure_model,
    runtime_poll, runtime_start, runtime_submit,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .invoke_handler(tauri::generate_handler![
            runtime_start,
            runtime_close,
            runtime_submit,
            runtime_command,
            runtime_cancel,
            runtime_poll,
            runtime_configure_model,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Medusa Desktop");
}
''',
)

write(
    tauri / "tauri.conf.json",
    r'''
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Medusa Desktop",
  "version": "1.0.0",
  "identifier": "com.benclawbot.medusa.desktop",
  "build": {
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build",
    "devUrl": "http://localhost:5173",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "Medusa Desktop",
        "fullscreen": false,
        "maximized": true,
        "width": 1440,
        "height": 920,
        "minWidth": 1024,
        "minHeight": 720,
        "resizable": true
      }
    ],
    "security": {
      "csp": {
        "default-src": "'self' customprotocol: asset:",
        "connect-src": "ipc: http://ipc.localhost",
        "img-src": "'self' asset: http://asset.localhost blob: data:",
        "style-src": "'self' 'unsafe-inline'",
        "font-src": "'self' data:",
        "object-src": "'none'",
        "frame-src": "'none'",
        "base-uri": "'self'",
        "form-action": "'none'"
      }
    }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "category": "DeveloperTool",
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "shortDescription": "Desktop interface for the Medusa coding agent",
    "longDescription": "Medusa Desktop is a Zeus-derived Tauri interface connected directly to Medusa's shared Rust runtime."
  }
}
''',
)

write(
    tauri / "capabilities/default.json",
    r'''
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Default permissions for the Medusa Desktop main window",
  "windows": ["main"],
  "permissions": ["core:default", "dialog:default"]
}
''',
)

for icon in ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"]:
    shutil.copyfile(zeus / "src-tauri/icons" / icon, tauri / "icons" / icon)

write(
    root / ".github/workflows/desktop.yml",
    r'''
name: Desktop

on:
  pull_request:
    paths:
      - "apps/medusa-desktop/**"
      - "crates/medusa-runtime/**"
      - ".github/workflows/desktop.yml"
  push:
    branches: [main]
    paths:
      - "apps/medusa-desktop/**"
      - "crates/medusa-runtime/**"
      - ".github/workflows/desktop.yml"

permissions:
  contents: read

concurrency:
  group: desktop-${{ github.ref }}
  cancel-in-progress: true

jobs:
  frontend:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: apps/medusa-desktop
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: "22"
          cache: npm
          cache-dependency-path: apps/medusa-desktop/package-lock.json
      - run: npm ci
      - run: npm run typecheck
      - run: npm test
      - run: npm run build

  rust-adapter:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-2022]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - name: Install Linux Tauri dependencies
        if: runner.os == 'Linux'
        run: sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
      - uses: dtolnay/rust-toolchain@1.88.0
        with:
          components: rustfmt,clippy
      - run: cargo fmt --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml -- --check
      - run: cargo clippy --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
      - run: cargo test --manifest-path apps/medusa-desktop/src-tauri/Cargo.toml --locked
''',
)

# Keep generated desktop artifacts out of the repository.
gitignore = root / ".gitignore"
ignore = gitignore.read_text()
for entry in [
    "/apps/medusa-desktop/node_modules/",
    "/apps/medusa-desktop/dist/",
    "/apps/medusa-desktop/src-tauri/target/",
]:
    if entry not in ignore:
        ignore += entry + "\n"
gitignore.write_text(ignore)

readme = root / "README.md"
text = readme.read_text()
section = r'''
## Desktop interface

`apps/medusa-desktop` is the Zeus-derived alternative entry point. It keeps the three-panel desktop shell and interaction model while replacing Zeus's separate agent implementation with a thin Tauri adapter over `medusa-runtime`.

```bash
cd apps/medusa-desktop
npm install
npm run tauri:dev
```

The desktop app opens a repository explicitly, then uses the same session controller, provider configuration, skills, cancellation, follow-up queue, plans, questions, tools, memory, and policy as the terminal entry point. File attachments are confined to the selected repository; pasted images are decoded and validated by the Rust adapter before entering the runtime.

'''
marker = "## Development and verification"
if "## Desktop interface" not in text:
    if marker not in text:
        raise SystemExit("README insertion marker missing")
    text = text.replace(marker, section + marker, 1)
readme.write_text(text)

ledger = root / "docs/CAPABILITY-EVIDENCE.md"
text = ledger.read_text()
row = "| Zeus-derived desktop entry point | `apps/medusa-desktop`; React/Tauri shell connected directly to `medusa-runtime` | Desktop frontend tests/build plus cross-platform Rust adapter Clippy/tests |\n"
anchor = "| Shared frontend-neutral interactive runtime |"
if row not in text:
    index = text.find(anchor)
    if index < 0:
        raise SystemExit("capability evidence runtime row missing")
    line_end = text.find("\n", index) + 1
    text = text[:line_end] + row + text[line_end:]
text = text.replace(
    "## Next architecture work\n\nThe frontend-neutral runtime extraction is shipped in PR #39. The Zeus-derived desktop interface is still **not shipped**; it must consume `medusa-runtime` rather than duplicate session, provider, cancellation, follow-up, or event logic, and it requires its own canonical validation before the evidence ledger can claim a desktop entry point.",
    "## Next architecture work\n\nThe frontend-neutral runtime extraction and the first Zeus-derived desktop entry point are shipped. Remaining desktop work should deepen parity—session discovery, richer diffs, approvals, memory browsing, installers, and accessibility—without reintroducing a separate agent engine or provider stack.",
)
ledger.write_text(text)
