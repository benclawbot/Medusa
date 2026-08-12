import React, { useState, useSyncExternalStore } from "react";
import { RecoveryPanel } from "./RecoveryPanel";
import {
  getRecoverySnapshot,
  getTimelineSnapshot,
  performRecoveryAction,
  subscribeRecovery,
  subscribeTimeline,
  type RecoveryOperation,
} from "./runtime";

export function RecoveryDock() {
  const recovery = useSyncExternalStore(subscribeRecovery, getRecoverySnapshot);
  const timeline = useSyncExternalStore(subscribeTimeline, getTimelineSnapshot);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  if (!recovery || !timeline.runtimeId) return null;

  const onAction = async (
    operation: RecoveryOperation,
    checkpointId?: string,
    confirmed?: boolean,
  ) => {
    setBusy(true);
    setError(undefined);
    try {
      await performRecoveryAction(timeline.runtimeId!, recovery, operation, checkpointId, confirmed);
    } catch (cause) {
      setError(String(cause));
      setBusy(false);
    }
  };

  return (
    <div className="recovery-dock" role="dialog" aria-modal="true" aria-label="Interrupted session recovery">
      <div className="recovery-dock-backdrop" />
      <div className="recovery-dock-content">
        {error && <div className="recovery-error" role="alert">{error}</div>}
        <RecoveryPanel recovery={recovery} busy={busy} onAction={onAction} />
      </div>
    </div>
  );
}
