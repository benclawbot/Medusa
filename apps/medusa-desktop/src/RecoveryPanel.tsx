import { AlertTriangle, CheckCircle2, RotateCcw, Search, ShieldAlert, XCircle } from "lucide-react";
import React, { useEffect, useMemo, useState } from "react";
import type { RecoveryOperation, RecoveryView } from "./runtime";

interface Props {
  recovery: RecoveryView;
  busy?: boolean;
  onAction: (operation: RecoveryOperation, checkpointId?: string, confirmed?: boolean) => Promise<void>;
}

export function RecoveryPanel({ recovery, busy = false, onAction }: Props) {
  const latestCheckpoint = recovery.checkpoints[recovery.checkpoints.length - 1];
  const defaultCheckpoint = recovery.selectedPreview?.checkpointId ?? latestCheckpoint?.id ?? "";
  const [checkpointId, setCheckpointId] = useState(defaultCheckpoint);
  const [confirmed, setConfirmed] = useState(false);

  useEffect(() => {
    setCheckpointId(defaultCheckpoint);
    setConfirmed(false);
  }, [defaultCheckpoint, recovery.sessionId]);

  const actionByOperation = useMemo(
    () => new Map(recovery.actions.map((item) => [item.operation, item])),
    [recovery.actions],
  );
  const selected = recovery.checkpoints.find((item) => item.id === checkpointId);
  const preview = recovery.selectedPreview?.checkpointId === checkpointId
    ? recovery.selectedPreview
    : undefined;
  const restore = actionByOperation.get("restoreCheckpoint");
  const restoreNeedsConfirmation = restore?.requiresConfirmation ?? false;
  const durableStep = recovery.lastDurableStep?.trim() || "the last durable checkpoint";
  const checkpointDate = selected ? new Date(selected.createdAtUnixMs) : undefined;
  const checkpointDateLabel = checkpointDate && Number.isFinite(checkpointDate.getTime())
    ? checkpointDate.toLocaleString()
    : "Date unavailable";
  const resume = actionByOperation.get("resume");
  const retry = actionByOperation.get("retryVerification");
  const nextStep = resume?.enabled
    ? "Resume the interrupted task to let Medusa continue from its last safe checkpoint."
    : retry?.enabled
      ? "Retry verification so Medusa can confirm the result before continuing."
      : "This interrupted task cannot be resumed automatically right now. Close this dialog and start a new session; your existing files are left untouched."

  const run = (operation: RecoveryOperation) => {
    const selectedCheckpoint = operation === "restoreCheckpoint" ? checkpointId : undefined;
    void onAction(operation, selectedCheckpoint, confirmed);
  };

  return (
    <section className={`recovery-panel recovery-${recovery.health.toLowerCase()}`} aria-label="Session recovery">
      <header>
        <div><p className="eyebrow">Interrupted session</p><h2>Recovery required</h2></div>
        <span className="recovery-health">{recovery.health}</span>
      </header>
      <p className="recovery-summary">
        Session <strong>{recovery.sessionId || "the interrupted session"}</strong> stopped after <strong>{durableStep}</strong>
        {recovery.interruptedOperation ? ` while ${recovery.interruptedOperation} was active` : ""}.
      </p>

      <section className="recovery-next-step" aria-label="Next step">
        <strong>What to do next</strong>
        <p>{nextStep}</p>
      </section>

      <dl className="recovery-facts">
        <div><dt>Verification</dt><dd>{recovery.verification}</dd></div>
        <div><dt>Checkpoints</dt><dd>{recovery.checkpoints.length}</dd></div>
        <div><dt>Approvals</dt><dd>{recovery.approvalsMustBeReestablished ? "Re-establish" : "Valid"}</dd></div>
        <div><dt>Containment</dt><dd>{recovery.containmentMustBeReestablished ? "Recheck" : "Ready"}</dd></div>
      </dl>

      {!!recovery.warnings.length && (
        <div className="recovery-warnings">
          {recovery.warnings.map((warning) => <p key={warning}><ShieldAlert size={15} />{warning}</p>)}
        </div>
      )}

      <label className="recovery-checkpoint">Checkpoint
        <select
          aria-label="Recovery checkpoint"
          value={checkpointId}
          onChange={(event) => { setCheckpointId(event.target.value); setConfirmed(false); }}
        >
          {recovery.checkpoints.map((checkpoint) => (
            <option key={checkpoint.id} value={checkpoint.id}>
              {checkpoint.taskStep} · #{checkpoint.sequence} · {checkpoint.integrityVerified ? "verified" : "untrusted"}
            </option>
          ))}
        </select>
      </label>

      {selected && (
        <details className="recovery-technical">
          <summary>Show checkpoint details</summary>
          <div className="checkpoint-card">
            <div><strong>{selected.reason || "Recovery checkpoint"}</strong><small>{checkpointDateLabel}</small></div>
              <p>Provenance: {selected.provenance || "unavailable"}</p>
            <p>Repository: <code>{(selected.repositoryFingerprint ?? "").slice(0, 12) || "unavailable"}</code></p>
          </div>
        </details>
      )}

      {preview ? (
        <div className="recovery-preview">
          <h3>Restore preview</h3>
          {preview.files.length ? preview.files.map((file) => (
            <p key={file.path}>
              {file.wouldOverwriteUncommittedWork ? <AlertTriangle size={14} /> : <CheckCircle2 size={14} />}
              <span>{file.kind}</span><code>{file.path}</code>
            </p>
          )) : <p>No file changes.</p>}
          {preview.unresolvedRisks.map((risk) => <p className="risk" key={risk}><XCircle size={14} />{risk}</p>)}
        </div>
      ) : (
        <p className="muted-copy">Select a checkpoint with a generated preview before restore. Inspection never modifies the working tree.</p>
      )}

      {restoreNeedsConfirmation && (
        <label className="recovery-confirm">
          <input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />
          I understand that recovery may overwrite uncommitted work.
        </label>
      )}

      <div className="recovery-actions">
        <button onClick={() => run("inspect")} disabled={busy || !actionByOperation.get("inspect")?.enabled}><Search size={15} />Inspect</button>
        <button className="primary" onClick={() => run("resume")} disabled={busy || !actionByOperation.get("resume")?.enabled}>Resume</button>
        <button onClick={() => run("retryVerification")} disabled={busy || !actionByOperation.get("retryVerification")?.enabled}>Retry verification</button>
        <button className="danger" onClick={() => run("restoreCheckpoint")} disabled={busy || !restore?.enabled || !checkpointId || (restoreNeedsConfirmation && !confirmed)}><RotateCcw size={15} />Restore</button>
        <button onClick={() => run("abandon")} disabled={busy || !actionByOperation.get("abandon")?.enabled}>Abandon</button>
      </div>
    </section>
  );
}
