import { CheckCircle2, ChevronDown, Circle, ListChecks, OctagonX } from "lucide-react";
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import {
  getTimelineSnapshot,
  subscribeTimeline,
  type PlanStep,
} from "./runtime";

export interface TodoCounts {
  completed: number;
  inProgress: number;
  pending: number;
}

/**
 * The DeepSeek harness presents its plan as a stable ordered to-do list: the
 * task titles stay put while only their status changes. Keep that same model
 * here instead of deriving user-facing rows from noisy tool/activity events.
 */
export function summarizePlan(plan: PlanStep[]): TodoCounts {
  let completed = 0;
  let inProgress = 0;

  for (const step of plan) {
    if (step.status === "completed") completed += 1;
    else if (step.status === "inProgress") inProgress += 1;
  }

  return {
    completed,
    inProgress,
    pending: plan.length - completed - inProgress,
  };
}

function TodoStatusIcon({ status }: { status: PlanStep["status"] }) {
  if (status === "completed") return <CheckCircle2 size={16} />;
  if (status === "inProgress") return <span className="todo-status-spinner" aria-hidden="true" />;
  if (status === "failed") return <OctagonX size={16} />;
  return <Circle size={15} />;
}

/**
 * Mount directly above the composer card. A dedicated mount keeps the to-do
 * surface out of the transcript, so tool-call churn never pushes chat content.
 */
function useTodoTarget(): HTMLElement | null {
  const [target, setTarget] = useState<HTMLElement | null>(null);

  useEffect(() => {
    const resolve = () => {
      const composer = document.querySelector<HTMLElement>(".composer-wrap");
      const card = composer?.querySelector<HTMLElement>(".composer-card");
      if (composer && card) {
        let mount = composer.querySelector<HTMLElement>("[data-medusa-todos]");
        if (!mount) {
          mount = document.createElement("div");
          mount.dataset.medusaTodos = "true";
          composer.insertBefore(mount, card);
        }
        setTarget(mount);
      }
    };

    resolve();
    // The composer is mounted conditionally during startup and repository switching. Observe
    // the DOM until it exists instead of giving up after a fixed timeout window.
    const observer = new MutationObserver(resolve);
    observer.observe(document.body, { childList: true, subtree: true });
    return () => {
      observer.disconnect();
    };
  }, []);

  return target;
}

export function DesktopTimelineBridge() {
  const target = useTodoTarget();
  const snapshot = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot, getTimelineSnapshot);
  const [expanded, setExpanded] = useState(false);
  const counts = useMemo(() => summarizePlan(snapshot.plan), [snapshot.plan]);

  if (!target || snapshot.plan.length === 0) return null;

  return createPortal(
    <section className={`todo-panel${expanded ? " expanded" : ""}`} aria-label="To-dos">
      <button
        type="button"
        className="todo-summary"
        aria-expanded={expanded}
        aria-controls="medusa-todo-list"
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="todo-heading"><ListChecks size={16} aria-hidden="true" /><strong>To-dos</strong></span>
        <span className="todo-counts">
          {counts.completed} completed <span aria-hidden="true">·</span> {counts.inProgress} in progress <span aria-hidden="true">·</span> {counts.pending} pending
        </span>
        <ChevronDown className="todo-chevron" size={17} aria-hidden="true" />
      </button>

      {expanded && (
        <div id="medusa-todo-list" className="todo-list" role="list">
          {snapshot.plan.map((step, index) => (
            <div className={`todo-row ${step.status}`} role="listitem" key={`${step.title}-${index}`}>
              <span className="todo-status" aria-hidden="true"><TodoStatusIcon status={step.status} /></span>
              <span className="todo-row-text" title={step.title}>{step.title}</span>
            </div>
          ))}
        </div>
      )}
    </section>,
    target,
  );
}
