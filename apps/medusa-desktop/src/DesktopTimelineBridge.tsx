import { Activity, CheckCircle2, ChevronDown, Circle, OctagonX, Play } from "lucide-react";
import React, { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";

interface TimelinePlanItem {
  title: string;
  status: string;
}

interface TimelineActivityItem {
  title: string;
  details: string[];
  kind: string;
}

interface TimelineSnapshot {
  plan: TimelinePlanItem[];
  activities: TimelineActivityItem[];
  busy: boolean;
}

const emptySnapshot: TimelineSnapshot = { plan: [], activities: [], busy: false };

function readSnapshot(): TimelineSnapshot {
  const plan = Array.from(document.querySelectorAll<HTMLElement>(".inspector .mini-plan > div")).map((item) => ({
    title: item.querySelector("span")?.textContent?.trim() ?? item.textContent?.trim() ?? "Plan step",
    status: item.className || "pending",
  }));

  const activities = Array.from(document.querySelectorAll<HTMLElement>(".inspector .activity-list > div"))
    .map((item) => ({
      title: item.querySelector("strong")?.textContent?.trim() ?? "Runtime activity",
      details: Array.from(item.querySelectorAll("small"))
        .map((detail) => detail.textContent?.trim() ?? "")
        .filter(Boolean),
      kind: item.className || "active",
    }))
    .reverse();

  return {
    plan,
    activities,
    busy: document.querySelector(".runtime-state .status-dot.busy") !== null,
  };
}

function sameSnapshot(left: TimelineSnapshot, right: TimelineSnapshot): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function PlanIcon({ status }: { status: string }) {
  if (status.includes("completed")) return <CheckCircle2 size={15} />;
  if (status.includes("failed")) return <OctagonX size={15} />;
  if (status.includes("inProgress")) return <Play size={14} />;
  return <Circle size={13} />;
}

function ActivityIcon({ kind }: { kind: string }) {
  if (kind.includes("error")) return <OctagonX size={15} />;
  if (kind.includes("done")) return <CheckCircle2 size={15} />;
  return <Activity size={15} />;
}

export function DesktopTimelineBridge() {
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const [snapshot, setSnapshot] = useState<TimelineSnapshot>(emptySnapshot);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  useEffect(() => {
    let frame = 0;
    const refresh = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        setTarget(document.querySelector<HTMLElement>(".transcript"));
        const next = readSnapshot();
        setSnapshot((current) => sameSnapshot(current, next) ? current : next);
      });
    };

    refresh();
    const observer = new MutationObserver(refresh);
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, characterData: true });
    return () => {
      observer.disconnect();
      window.cancelAnimationFrame(frame);
    };
  }, []);

  const completed = useMemo(
    () => snapshot.plan.filter((item) => item.status.includes("completed")).length,
    [snapshot.plan],
  );

  if (!target || (!snapshot.plan.length && !snapshot.activities.length && !snapshot.busy)) return null;

  return createPortal(
    <section className="conversation-timeline" aria-label="Live execution timeline">
      <header className="timeline-header">
        <div>
          <span className={`timeline-live-dot${snapshot.busy ? " busy" : ""}`} />
          <strong>{snapshot.busy ? "Medusa is working" : "Execution timeline"}</strong>
        </div>
        {!!snapshot.plan.length && <small>{completed}/{snapshot.plan.length} steps complete</small>}
      </header>

      {!!snapshot.plan.length && (
        <div className="timeline-plan" aria-label="Execution plan">
          {snapshot.plan.map((item, index) => (
            <div className={`timeline-plan-step ${item.status}`} key={`${item.title}-${index}`}>
              <span className="timeline-node"><PlanIcon status={item.status} /></span>
              <span>{item.title}</span>
            </div>
          ))}
        </div>
      )}

      {!!snapshot.activities.length && (
        <div className="timeline-activity" aria-label="Tool activity">
          {snapshot.activities.map((item, index) => {
            const key = `${item.title}-${index}`;
            const isExpanded = expanded[key] ?? item.kind.includes("error");
            return (
              <article className={`timeline-activity-card ${item.kind}`} key={key}>
                <button
                  type="button"
                  aria-expanded={isExpanded}
                  onClick={() => setExpanded((current) => ({ ...current, [key]: !isExpanded }))}
                >
                  <span className="timeline-activity-icon"><ActivityIcon kind={item.kind} /></span>
                  <span className="timeline-activity-title">{item.title}</span>
                  {!!item.details.length && <ChevronDown className={isExpanded ? "expanded" : ""} size={15} />}
                </button>
                {isExpanded && !!item.details.length && (
                  <div className="timeline-activity-details">
                    {item.details.map((detail, detailIndex) => <small key={`${detail}-${detailIndex}`}>{detail}</small>)}
                  </div>
                )}
              </article>
            );
          })}
        </div>
      )}

      {snapshot.busy && (
        <div className="timeline-progress" aria-label="Work in progress">
          <span />
        </div>
      )}
    </section>,
    target,
  );
}
