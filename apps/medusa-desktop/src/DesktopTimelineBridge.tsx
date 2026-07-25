import { Activity, CheckCircle2, ChevronDown, Circle, OctagonX, Play } from "lucide-react";
import React, { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import {
  getTimelineSnapshot,
  subscribeTimeline,
  type PlanStep,
  type RuntimeActivity,
} from "./runtime";

function PlanIcon({ status }: { status: PlanStep["status"] }) {
  if (status === "completed") return <CheckCircle2 size={15} />;
  if (status === "failed") return <OctagonX size={15} />;
  if (status === "inProgress") return <Play size={14} />;
  return <Circle size={13} />;
}

function ActivityIcon({ kind }: { kind: RuntimeActivity["kind"] }) {
  if (kind === "error") return <OctagonX size={15} />;
  if (kind === "done") return <CheckCircle2 size={15} />;
  return <Activity size={15} />;
}

function useTranscriptTarget(): HTMLElement | null {
  const [target, setTarget] = useState<HTMLElement | null>(null);

  useEffect(() => {
    let frame = 0;
    const resolve = () => {
      const transcript = document.querySelector<HTMLElement>(".transcript");
      if (transcript) {
        setTarget(transcript);
        return;
      }
      frame = window.requestAnimationFrame(resolve);
    };
    resolve();
    return () => window.cancelAnimationFrame(frame);
  }, []);

  return target;
}

export function DesktopTimelineBridge() {
  const target = useTranscriptTarget();
  const snapshot = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot, getTimelineSnapshot);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const completed = useMemo(
    () => snapshot.plan.filter((item) => item.status === "completed").length,
    [snapshot.plan],
  );

  if (!target || (!snapshot.plan.length && !snapshot.activities.length && !snapshot.busy)) return null;

  return createPortal(
    <section className="conversation-timeline" aria-label="Live execution timeline" aria-live="polite">
      <header className="timeline-header">
        <div>
          <span className={`timeline-live-dot${snapshot.busy ? " busy" : ""}`} aria-hidden="true" />
          <strong>{snapshot.busy ? "Medusa is working" : "Execution timeline"}</strong>
        </div>
        {!!snapshot.plan.length && <small>{completed}/{snapshot.plan.length} steps complete</small>}
      </header>

      {!!snapshot.plan.length && (
        <div className="timeline-plan" aria-label="Execution plan">
          {snapshot.plan.map((item, index) => (
            <div className={`timeline-plan-step ${item.status}`} key={`${item.title}-${index}`}>
              <span className="timeline-node" aria-hidden="true"><PlanIcon status={item.status} /></span>
              <span>{item.title}</span>
            </div>
          ))}
        </div>
      )}

      {!!snapshot.activities.length && (
        <div className="timeline-activity" aria-label="Tool activity">
          {snapshot.activities.slice(-12).reverse().map((item, index) => {
            const key = item.id ?? `${item.title}-${index}`;
            const isExpanded = expanded[key] ?? item.kind === "error";
            return (
              <article className={`timeline-activity-card ${item.kind}`} key={key}>
                <button
                  type="button"
                  aria-expanded={isExpanded}
                  aria-controls={`timeline-details-${key}`}
                  onClick={() => setExpanded((current) => ({ ...current, [key]: !isExpanded }))}
                >
                  <span className="timeline-activity-icon" aria-hidden="true"><ActivityIcon kind={item.kind} /></span>
                  <span className="timeline-activity-title">{item.title}</span>
                  {!!item.details.length && <ChevronDown className={isExpanded ? "expanded" : ""} size={15} aria-hidden="true" />}
                </button>
                {isExpanded && !!item.details.length && (
                  <div className="timeline-activity-details" id={`timeline-details-${key}`}>
                    {item.details.map((detail, detailIndex) => <small key={`${detail}-${detailIndex}`}>{detail}</small>)}
                  </div>
                )}
              </article>
            );
          })}
        </div>
      )}

      {snapshot.busy && (
        <div className="timeline-progress" aria-label="Work in progress" role="progressbar">
          <span />
        </div>
      )}
    </section>,
    target,
  );
}
