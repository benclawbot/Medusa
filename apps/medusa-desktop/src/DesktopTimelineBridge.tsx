import {
  Activity,
  CheckCircle2,
  ChevronDown,
  Circle,
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
import { emptyTimelineSnapshot, type TimelineEvent } from "./timeline/model";
import { reduceTimelineEvents } from "./timeline/reducer";

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

function useTimelineTarget(): HTMLElement | null {
  const [target, setTarget] = useState<HTMLElement | null>(null);

  useEffect(() => {
    let frame = 0;
    const resolve = () => {
      const anchor = document.querySelector<HTMLElement>(".timeline-anchor");
      if (anchor) {
        setTarget(anchor);
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

function toStructuredEvents(plan: PlanStep[], activities: RuntimeActivity[], busy: boolean) {
  const runtimeEvents: RuntimeEvent[] = [
    ...(busy ? [{ type: "started" } as RuntimeEvent] : []),
    { type: "plan", steps: plan },
    ...activities.map((activity): RuntimeEvent => ({ type: "activity", activity })),
  ];
  return reduceTimelineEvents(emptyTimelineSnapshot, runtimeEvents);
}

export function DesktopTimelineBridge() {
  const transcript = useTranscriptTarget();
  const target = useTimelineTarget();
  const legacySnapshot = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot, getTimelineSnapshot);
  const structured = useMemo(
    () => toStructuredEvents(legacySnapshot.plan, legacySnapshot.activities, legacySnapshot.busy),
    [legacySnapshot.plan, legacySnapshot.activities, legacySnapshot.busy],
  );
  const { following, jumpToLatest } = useScrollGuard(transcript, structured.revision);

  const visibleEvents = useMemo(
    () => (structured.events ?? []).slice(-18),
    [structured.events],
  );
  const activityEvents = visibleEvents.filter((event) => event.kind === "activity" || event.kind === "verification");

  if (!target || (!structured.plan.length && !structured.events.length && !structured.busy)) return null;

  return createPortal(
    <>
      <section className="conversation-timeline" aria-label="Live execution timeline" aria-live="polite">
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
          <section className="timeline-activity" aria-label="Tool calls">
            {activityEvents.map((event) => (
              <div className={`timeline-activity-row ${event.status}`} key={event.id}>
                <span className="timeline-activity-icon" aria-hidden="true"><ActivityIcon event={event} /></span>
                <span className="timeline-activity-title">{event.title}</span>
                {!!event.details[0] && <span className="timeline-activity-detail">{event.details[0]}</span>}
              </div>
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
    </>,
    target,
  );
}
