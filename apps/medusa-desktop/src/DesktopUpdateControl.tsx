import { CheckCircle2, Download, RefreshCw, TriangleAlert } from "lucide-react";
import React, { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";

interface DesktopUpdateStatus {
  currentVersion: string;
  latestMainSha: string;
  executable: string;
  ready: boolean;
  missingDependencies: string[];
}

export function DesktopUpdateControl() {
  const [target, setTarget] = useState<Element | null>(null);
  const [status, setStatus] = useState<DesktopUpdateStatus>();
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    const findTarget = () => setTarget(document.querySelector(".settings-form"));
    findTarget();
    const observer = new MutationObserver(findTarget);
    observer.observe(document.body, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, []);

  const check = async () => {
    setChecking(true);
    setError(undefined);
    try {
      setStatus(await invoke<DesktopUpdateStatus>("desktop_update_status"));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setChecking(false);
    }
  };

  const update = async () => {
    setUpdating(true);
    setError(undefined);
    try {
      await invoke("desktop_update_from_main");
    } catch (cause) {
      setUpdating(false);
      setError(String(cause));
    }
  };

  if (!target) return null;

  return createPortal(
    <section className="desktop-update-card" aria-label="Desktop updates">
      <div className="desktop-update-heading">
        <span className="desktop-update-icon"><Download size={17} /></span>
        <div>
          <h3>Desktop updates</h3>
          <p>Build and install the latest Medusa Desktop directly from <code>main</code>.</p>
        </div>
      </div>

      {status && (
        <div className="desktop-update-status">
          <span><CheckCircle2 size={14} /> Installed v{status.currentVersion}</span>
          <span>Latest main: <code>{status.latestMainSha.slice(0, 8)}</code></span>
        </div>
      )}

      {status && !status.ready && (
        <div className="desktop-update-warning">
          <TriangleAlert size={15} /> Install {status.missingDependencies.join(", ")} to update from source.
        </div>
      )}

      {error && <div className="desktop-update-warning"><TriangleAlert size={15} /> {error}</div>}

      <div className="desktop-update-actions">
        <button className="secondary-action" onClick={check} disabled={checking || updating}>
          <RefreshCw size={14} className={checking ? "spin" : ""} />
          {checking ? "Checking…" : "Check main"}
        </button>
        <button className="primary-action" onClick={update} disabled={!status?.ready || checking || updating}>
          <Download size={14} />
          {updating ? "Preparing restart…" : "Update and restart"}
        </button>
      </div>
      <small>The app closes, builds the latest main branch, replaces this executable, and reopens automatically.</small>
    </section>,
    target,
  );
}
