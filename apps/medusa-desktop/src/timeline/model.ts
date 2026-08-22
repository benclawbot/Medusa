import { visibleAssistantText, type DesktopAttachment, type PlanStep, type RuntimeActivity, type RuntimeEvent } from "../runtime";

export type TimelineDensity = "focused" | "balanced" | "diagnostic";
export type TimelineRole = "user" | "assistant" | "system";
export type TimelineStatus = "pending" | "running" | "succeeded" | "failed" | "cancelled" | "blocked" | "skipped";
export type TimelineAttention = "none" | "info" | "warning" | "required";
export type TimelineEventKind =
  | "message"
  | "activity"
  | "verification"
  | "question"
  | "notice"
  | "completion";

export interface TimelineBaseEvent {
  id: string;
  sequence: number;
  kind: TimelineEventKind;
  status: TimelineStatus;
  attention: TimelineAttention;
  title: string;
  details: string[];
  parentId?: string;
  sourceActivityId?: string;
}

export interface TimelineMessageEvent extends TimelineBaseEvent {
  kind: "message";
  role: TimelineRole;
  text: string;
  attachments?: DesktopAttachment[];
  queued?: boolean;
}

export interface TimelineActivityEvent extends TimelineBaseEvent {
  kind: "activity" | "verification";
  activityKind: RuntimeActivity["kind"];
}

export interface TimelineQuestionEvent extends TimelineBaseEvent {
  kind: "question";
}

export interface TimelineNoticeEvent extends TimelineBaseEvent {
  kind: "notice" | "completion";
}

export type TimelineEvent =
  | TimelineMessageEvent
  | TimelineActivityEvent
  | TimelineQuestionEvent
  | TimelineNoticeEvent;

export interface TimelineSnapshot {
  events: TimelineEvent[];
  plan: PlanStep[];
  busy: boolean;
  revision: number;
  nextSequence: number;
}

export const emptyTimelineSnapshot: TimelineSnapshot = {
  events: [],
  plan: [],
  busy: false,
  revision: 0,
  nextSequence: 1,
};

export interface ProjectRuntimeEventOptions {
  fallbackId?: string;
}

function activityStatus(activity: RuntimeActivity): TimelineStatus {
  if (activity.kind === "error") return "failed";
  if (activity.kind === "done") return "succeeded";
  return "running";
}

export function projectRuntimeEvent(
  event: RuntimeEvent,
  sequence: number,
  options: ProjectRuntimeEventOptions = {},
): TimelineEvent | undefined {
  const fallbackId = options.fallbackId ?? `runtime-${sequence}`;
  switch (event.type) {
    case "assistantText":
      {
        const text = visibleAssistantText(event.text);
        if (!text) return undefined;
      return {
        id: fallbackId,
        sequence,
        kind: "message",
        role: "assistant",
        status: "succeeded",
        attention: "none",
        title: "Medusa",
        text,
        details: [],
      };
      }
    case "activity": {
      const verification = event.activity.kind === "verification";
      return {
        id: event.activity.id ? `activity-${event.activity.id}` : fallbackId,
        sourceActivityId: event.activity.id,
        sequence,
        kind: verification ? "verification" : "activity",
        activityKind: event.activity.kind,
        status: activityStatus(event.activity),
        attention: event.activity.kind === "error" ? "required" : "none",
        title: event.activity.title,
        details: event.activity.details ?? [],
      };
    }
    case "notice":
      return {
        id: fallbackId,
        sequence,
        kind: "notice",
        status: "succeeded",
        attention: "info",
        title: event.title,
        details: event.details ?? [],
      };
    case "compacted":
      return {
        id: fallbackId,
        sequence,
        kind: "notice",
        status: "succeeded",
        attention: "info",
        title: "Context compacted",
        details: [event.message],
      };
    case "completed":
      return {
        id: fallbackId,
        sequence,
        kind: "completion",
        status: "succeeded",
        attention: "info",
        title: "Session completed",
        details: [event.sessionId],
      };
    case "cancelled":
      return {
        id: fallbackId,
        sequence,
        kind: "notice",
        status: "cancelled",
        attention: "warning",
        title: "Turn cancelled",
        details: [],
      };
    case "failed":
      return {
        id: fallbackId,
        sequence,
        kind: "notice",
        status: "failed",
        attention: "required",
        title: "Runtime failed",
        details: [event.message],
      };
    case "question":
      return {
        id: fallbackId,
        sequence,
        kind: "question",
        status: "blocked",
        attention: "required",
        title: "Input required",
        details: event.prompts.map((prompt) => prompt.question),
      };
    default:
      return undefined;
  }
}
