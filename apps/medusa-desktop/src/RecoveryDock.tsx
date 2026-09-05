import React, { useState, useSyncExternalStore } from "react";
import {
  dismissRecovery,
  getRecoverySnapshot,
  getTimelineSnapshot,
  performRecoveryAction,
  runRuntimeCommand,
  subscribeRecovery,
  subscribeTimeline,
  type RecoveryOperation,
} from "./runtime";
import { toUserError } from "./errorPresentation";

/**
 * Recovery is shown alongside the work log. The user needs a short explanation
 * and one safe next step, not a blocking technical dialog full of checkpoint data.
 */
export function RecoveryDock() {
  const recovery = useSyncExternalStore(subscribeRecovery, getRecoverySnapshot);
  const timeline = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  if (!recovery || !timeline.runtimeId) return null;

  const resumeEnabled = recovery.actions.some((action) => action.operation === "resume" && action.enabled);
  const retryEnabled = recovery.actions.some((action) => action.operation === "retryVerification" && action.enabled);
  const recoveryOperation: RecoveryOperation | undefined = resumeEnabled
    ? "resume"
    : retryEnabled
      ? "retryVerification"
      : undefined;
  const durableStep = recovery.lastDurableStep?.trim();
  const explanation = durableStep
    ? `Medusa stopped after ${durableStep}, before it could verify a completed result.`
    : "Medusa stopped before it could verify a completed result.";

  const takeNextStep = async () => {
    setBusy(true);
    setError(undefined);
    try {
      if (recoveryOperation) {
        await performRecoveryAction(timeline.runtimeId!, recovery, recoveryOperation);
      } else {
        await runRuntimeCommand(timeline.runtimeId!, "/new");
      }
      dismissRecovery();
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="recovery-inline" role="status" aria-live="polite" aria-label="Task stopped">
      <span className="recovery-inline-mark" aria-hidden="true">!</span>
      <div className="recovery-inline-copy">
        <strong>{recoveryOperation === "resume" ? "Task paused" : "Task stopped"}</strong>
        <span>{explanation} {recoveryOperation === "resume" ? "Medusa found a safe continuation point." : recoveryOperation === "retryVerification" ? "Medusa can retry its checks before continuing." : "The existing files were left untouched."}</span>
        {error && <small role="alert">{error}</small>}
      </div>
      <button className="recovery-inline-action" onClick={() => void takeNextStep()} disabled={busy}>
        {busy ? "Working…" : recoveryOperation === "resume" ? "Resume task" : recoveryOperation === "retryVerification" ? "Retry checks" : "Start new session"}
      </button>
      <button className="recovery-inline-dismiss" onClick={dismissRecovery} disabled={busy} aria-label="Dismiss task status">Dismiss</button>
    </aside>
  );
}
