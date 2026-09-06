import { CheckCircle2, Download, RefreshCw, TriangleAlert } from "lucide-react";
import React, { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useDesktopSlots } from "./DesktopSlots";
import { toUserError } from "./errorPresentation";

interface DesktopUpdateStatus {
  currentVersion: string;
  latestMainSha?: string;
  executable: string;
  ready: boolean;
  artifactPublished: boolean;
}

interface DesktopUpdateProgress {
  phase: "downloading" | "installing" | "replacing" | "restarting" | "failed" | "preparing";
  completed: number;
  total?: number | null;
  message: string;
}

function progressPercent(progress: DesktopUpdateProgress): number {
  if (progress.total && progress.total > 0) {
    return Math.min(100, Math.round((progress.completed / progress.total) * 100));
  }
  if (progress.phase === "installing") return 92;
  if (progress.phase === "replacing") return 99;
  if (progress.phase === "restarting") return 100;
  return 5;
}

export function DesktopUpdateControl() {
  const { updateTarget: target } = useDesktopSlots();
  const [status, setStatus] = useState<DesktopUpdateStatus>();
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [progress, setProgress] = useState<DesktopUpdateProgress>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void listen<DesktopUpdateProgress>("desktop-update-progress", (event) => {
      if (!active) return;
      setProgress(event.payload);
      if (event.payload.phase === "failed") {
        setUpdating(false);
        setError(event.payload.message);
      }
    })
      .then((stop) => {
        if (active) {
          unlisten = stop;
        } else {
          stop();
        }
      })
      .catch(() => {
        // The component is also rendered by the browser test harness, where Tauri events
        // are unavailable. Native builds still receive the event listener above.
      });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  const check = async () => {
    setChecking(true);
    setError(undefined);
    setProgress(undefined);
    try {
      setStatus(await invoke<DesktopUpdateStatus>("desktop_update_status"));
    } catch (cause) {
      setError(toUserError(cause));
    } finally {
      setChecking(false);
    }
  };

  const update = async () => {
    if (!status?.latestMainSha) {
      setError("The checked main revision is unavailable; check again shortly.");
      return;
    }
    setUpdating(true);
    setError(undefined);
    setProgress({
      phase: "preparing",
      completed: 0,
      message: "Preparing the verified desktop update…",
    });
    try {
      await invoke("desktop_update_from_main", { targetSha: status.latestMainSha });
    } catch (cause) {
      setUpdating(false);
      setProgress(undefined);
      setError(toUserError(cause));
    }
  };

  if (!target) return null;

  const percentage = progress ? progressPercent(progress) : 0;

  return createPortal(
    <section className="desktop-update-card" aria-label="Desktop updates">
      <div className="desktop-update-heading">
        <span className="desktop-update-icon"><Download size={17} /></span>
        <div>
          <h3>Desktop updates</h3>
          <p>{updating
            ? "Medusa Desktop stays open while the verified download is prepared."
            : <>Download and install the checked, prebuilt Medusa Desktop revision from <code>main</code>.</>}</p>
        </div>
      </div>

      {status && (
        <div className="desktop-update-status">
          <span><CheckCircle2 size={14} /> Installed v{status.currentVersion}</span>
          <span>Checked main: <code>{status.latestMainSha ? status.latestMainSha.slice(0, 8) : "unavailable"}</code></span>
        </div>
      )}

      {status && !status.artifactPublished && (
        <div className="desktop-update-warning">
          <TriangleAlert size={15} /> The checked revision is still being published; check again shortly.
        </div>
      )}

      {error && <div className="desktop-update-warning"><TriangleAlert size={15} /> {error}</div>}

      {updating && progress && (
        <div className="desktop-update-progress" role="status" aria-live="polite">
          <div className="desktop-update-progress-label">
            <span>{progress.message}</span>
            <strong>{percentage}%</strong>
          </div>
          <div
            className="desktop-update-progress-track"
            role="progressbar"
            aria-label="Desktop update progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percentage}
            aria-valuetext={`${progress.message} ${percentage}%`}
          >
            <span style={{ width: `${percentage}%` }} />
          </div>
        </div>
      )}

      <div className="desktop-update-actions">
        <button className="secondary-action" onClick={check} disabled={checking || updating}>
          <RefreshCw size={14} className={checking ? "spin" : ""} />
          {checking ? "Checking…" : "Check main"}
        </button>
        <button className="primary-action" onClick={update} disabled={!status?.ready || checking || updating}>
          <Download size={14} />
          {updating
            ? progress?.phase === "replacing" ? "Closing to update…" : "Updating…"
            : "Update and restart"}
        </button>
      </div>
      <small>The app verifies the exact published executable and shows download progress. It closes only for the final replacement, then reopens automatically.</small>
    </section>,
    target,
  );
}
