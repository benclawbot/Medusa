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

export async function startRuntime(repo?: string): Promise<RuntimeStartResponse> {
  return invoke<RuntimeStartResponse>("runtime_start", repo ? { repo } : {});
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
