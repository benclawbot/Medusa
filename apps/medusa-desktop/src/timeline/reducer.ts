import type { RuntimeEvent } from "../runtime";
import {
  emptyTimelineSnapshot,
  projectRuntimeEvent,
  type TimelineEvent,
  type TimelineSnapshot,
} from "./model";

function upsertEvent(events: TimelineEvent[], event: TimelineEvent): TimelineEvent[] {
  const index = events.findIndex((candidate) => candidate.id === event.id);
  if (index < 0) return [...events, event];
  const next = [...events];
  next[index] = { ...event, sequence: events[index].sequence };
  return next;
}

export function reduceTimelineEvent(
  snapshot: TimelineSnapshot,
  event: RuntimeEvent,
): TimelineSnapshot {
  if (event.type === "newSession") {
    return { ...emptyTimelineSnapshot, revision: snapshot.revision + 1 };
  }

  let busy = snapshot.busy;
  let plan = snapshot.plan;

  switch (event.type) {
    case "started":
      busy = true;
      break;
    case "plan":
      plan = event.steps;
      break;
    case "question":
    case "completed":
    case "turnFinished":
    case "cancelled":
    case "failed":
      busy = false;
      break;
    default:
      break;
  }

  const projected = projectRuntimeEvent(event, snapshot.nextSequence);
  const events = projected ? upsertEvent(snapshot.events, projected) : snapshot.events;
  const consumedSequence = projected && !snapshot.events.some((item) => item.id === projected.id);

  if (events === snapshot.events && plan === snapshot.plan && busy === snapshot.busy) {
    return snapshot;
  }

  return {
    events,
    plan,
    busy,
    revision: snapshot.revision + 1,
    nextSequence: consumedSequence ? snapshot.nextSequence + 1 : snapshot.nextSequence,
  };
}

export function reduceTimelineEvents(
  snapshot: TimelineSnapshot,
  events: RuntimeEvent[],
): TimelineSnapshot {
  return events.reduce(reduceTimelineEvent, snapshot);
}
