import {
  Activity,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  Circle,
  Gauge,
  OctagonX,
  Play,
  ShieldCheck,
} from "lucide-react";
import React, { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import {
  getTimelineSnapshot,
  subscribeTimeline,
  type PlanStep,
  type RuntimeActivity,
  type RuntimeEvent,
} from "./runtime";
import { emptyTimelineSnapshot, type TimelineDensity, type TimelineEvent } from "./timeline/model";
import { reduceTimelineEvents } from "./timeline/reducer";

const densityStorageKey = "medusa.desktop.timelineDensity";

function PlanIcon({ status }: { status: PlanStep["status"] }) {
  if (status === "completed") return <CheckCircle2 size={15} />;
  if (status === "failed") return <OctagonX size={15} />;
  if (status === "inProgress") return <Play size={14} />;
  return <Circle size={13} />;
}

function ActivityIcon({ event }: { event: TimelineEvent }) {
  if (event.status === "failed") return <OctagonX size={15} />;
  if (event.status === "succeeded") return <CheckCircle2 size={15} />;
  if (event.kind === "verification") return <ShieldCheck size={15} />;
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

function useScrollGuard(target: HTMLElement | null, revision: number) {
  const [following, setFollowing] = useState(true);
  const originalScrollTo = useRef<HTMLElement["scrollTo"]>();

  useEffect(() => {
    if (!target) return;
    const threshold = 72;
    const nearBottom = () => target.scrollHeight - target.scrollTop - target.clientHeight <= threshold;
    const updateFollowing = () => setFollowing(nearBottom());
    const nativeScrollTo = target.scrollTo.bind(target);
    originalScrollTo.current = nativeScrollTo;
    target.scrollTo = ((optionsOrX?: ScrollToOptions | number, y?: number) => {
      if (!nearBottom()) return;
      if (typeof optionsOrX === "number") nativeScrollTo(optionsOrX, y ?? 0);
      else nativeScrollTo(optionsOrX);
    }) as HTMLElement["scrollTo"];
    target.addEventListener("scroll", updateFollowing, { passive: true });
    updateFollowing();
    return () => {
      target.removeEventListener("scroll", updateFollowing);
      target.scrollTo = nativeScrollTo;
    };
  }, [target]);

  useEffect(() => {
    if (!target || !following) return;
    window.requestAnimationFrame(() => {
      originalScrollTo.current?.({ top: target.scrollHeight, behavior: "smooth" });
    });
  }, [target, following, revision]);

  const jumpToLatest = () => {
    if (!target) return;
    originalScrollTo.current?.({ top: target.scrollHeight, behavior: "smooth" });
    setFollowing(true);
  };

  return { following, jumpToLatest };
}

function loadDensity(): TimelineDensity {
  const stored = window.localStorage.getItem(densityStorageKey);
  return stored === "focused" || stored === "diagnostic" ? stored : "balanced";
}

function eventIsVisible(event: TimelineEvent, density: TimelineDensity): boolean {
  if (density === "diagnostic") return true;
  if (event.status === "failed" || event.attention === "required" || event.kind === "verification") return true;
  if (density === "focused") return false;
  return event.kind === "activity";
}

function toStructuredEvents(plan: PlanStep[], activities: RuntimeActivity[], busy: boolean) {
  const runtimeEvents: RuntimeEvent[] = [
    ...(busy ? [{ type: "started" } as RuntimeEvent] : []),
    { type: "plan", steps: plan },
    ...activities.map((activity): RuntimeEvent => ({ type: "activity", activity })),
  ];
  return reduceTimelineEvents(emptyTimelineSnapshot, runtimeEvents);
}

export function DesktopTimelineBridge() {
  const target = useTranscriptTarget();
  const legacySnapshot = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot, getTimelineSnapshot);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [density, setDensity] = useState<TimelineDensity>(loadDensity);
  const structured = useMemo(
    () => toStructuredEvents(legacySnapshot.plan, legacySnapshot.activities, legacySnapshot.busy),
    [legacySnapshot.plan, legacySnapshot.activities, legacySnapshot.busy],
  );
  const { following, jumpToLatest } = useScrollGuard(target, structured.revision);

  const completed = useMemo(
    () => structured.plan.filter((item) => item.status === "completed").length,
    [structured.plan],
  );
  const visibleEvents = useMemo(
    () => (structured.events ?? []).filter((event) => eventIsVisible(event, density)).slice(-18),
    [structured.events, density],
  );
  const verificationEvents = visibleEvents.filter((event) => event.kind === "verification");
  const activityEvents = visibleEvents.filter((event) => event.kind !== "verification");

  const setTimelineDensity = (next: TimelineDensity) => {
    setDensity(next);
    window.localStorage.setItem(densityStorageKey, next);
  };

  if (!target || (!structured.plan.length && !structured.events.length && !structured.busy)) return null;

  return createPortal(
    <>
      <section className="conversation-timeline" aria-label="Live execution timeline" aria-live="polite">
        <header className="timeline-header">
          <div>
            <span className={`timeline-live-dot${structured.busy ? " busy" : ""}`} aria-hidden="true" />
            <strong>{structured.busy ? "Medusa is working" : "Execution timeline"}</strong>
          </div>
          <div className="timeline-header-actions">
            {!!structured.plan.length && <small>{completed}/{structured.plan.length} steps complete</small>}
            <label className="timeline-density-label">
              <Gauge size={13} aria-hidden="true" />
              <span className="sr-only">Timeline density</span>
              <select value={density} onChange={(event) => setTimelineDensity(event.target.value as TimelineDensity)}>
                <option value="focused">Focused</option>
                <option value="balanced">Balanced</option>
                <option value="diagnostic">Diagnostic</option>
              </select>
            </label>
          </div>
        </header>

        {!!structured.plan.length && (
          <div className="timeline-plan" aria-label="Execution plan">
            {structured.plan.map((item, index) => (
              <div className={`timeline-plan-step ${item.status}`} key={`${item.title}-${index}`}>
                <span className="timeline-node" aria-hidden="true"><PlanIcon status={item.status} /></span>
                <span>{item.title}</span>
              </div>
            ))}
          </div>
        )}

        {!!activityEvents.length && (
          <section className="timeline-group" aria-label="Grouped execution activity">
            <div className="timeline-group-heading">
              <span>Execution activity</span>
              <small>{activityEvents.length} action{activityEvents.length === 1 ? "" : "s"}</small>
            </div>
            <div className="timeline-activity">
              {activityEvents.map((event) => {
                const defaultExpanded = event.status === "failed" || (structured.busy && event.status === "running");
                const isExpanded = expanded[event.id] ?? defaultExpanded;
                return (
                  <article className={`timeline-activity-card ${event.status}`} key={event.id}>
                    <button
                      type="button"
                      aria-expanded={isExpanded}
                      aria-controls={`timeline-details-${event.id}`}
                      onClick={() => setExpanded((current) => ({ ...current, [event.id]: !isExpanded }))}
                    >
                      <span className="timeline-activity-icon" aria-hidden="true"><ActivityIcon event={event} /></span>
                      <span className="timeline-activity-title">{event.title}</span>
                      <span className={`timeline-status ${event.status}`}>{event.status}</span>
                      {!!event.details.length && <ChevronDown className={isExpanded ? "expanded" : ""} size={15} aria-hidden="true" />}
                    </button>
                    {isExpanded && !!event.details.length && (
                      <div className="timeline-activity-details" id={`timeline-details-${event.id}`}>
                        {event.details.map((detail, detailIndex) => <small key={`${detail}-${detailIndex}`}>{detail}</small>)}
                      </div>
                    )}
                  </article>
                );
              })}
            </div>
          </section>
        )}

        {!!verificationEvents.length && (
          <section className="timeline-verification" aria-label="Verification evidence">
            <div className="timeline-verification-heading"><ShieldCheck size={15} /> Verification</div>
            {verificationEvents.map((event) => (
              <article className={`verification-card ${event.status}`} key={event.id}>
                <div>
                  <strong>{event.title}</strong>
                  <span>{event.status === "succeeded" ? "Evidence recorded" : event.status}</span>
                </div>
                {!!event.details.length && (
                  <ul>{event.details.map((detail, index) => <li key={`${detail}-${index}`}>{detail}</li>)}</ul>
                )}
              </article>
            ))}
          </section>
        )}

        {structured.busy && (
          <div className="timeline-progress" aria-label="Work in progress" role="progressbar">
            <span />
          </div>
        )}
      </section>

      {!following && (
        <button type="button" className="timeline-jump-latest" onClick={jumpToLatest}>
          <ChevronDown size={15} /> New activity below
        </button>
      )}
      {following && structured.busy && (
        <span className="timeline-following" aria-hidden="true"><ChevronUp size={12} /> following live activity</span>
      )}
    </>,
    target,
  );
}
