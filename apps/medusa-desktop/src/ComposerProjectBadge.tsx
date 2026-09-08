import { FolderOpen } from "lucide-react";
import { useSyncExternalStore } from "react";
import { REPO_CHANGED_EVENT } from "./runtime";

function subscribe(listener: () => void): () => void {
  window.addEventListener(REPO_CHANGED_EVENT, listener);
  window.addEventListener("focus", listener);
  return () => {
    window.removeEventListener(REPO_CHANGED_EVENT, listener);
    window.removeEventListener("focus", listener);
  };
}

function snapshot(): string {
  return window.localStorage.getItem("medusa.desktop.repo") ?? "";
}

function projectName(path: string): string {
  const normalized = path.replace(/[\\/]+$/, "");
  return normalized.split(/[\\/]/).pop() || "General chat";
}

/** Lightweight context label positioned in the composer chrome. */
export function ComposerProjectBadge() {
  const repo = useSyncExternalStore(subscribe, snapshot, snapshot);
  if (!repo) return null;
  return (
    <div className="composer-project-badge" aria-label={`Current project: ${projectName(repo)}`}>
      <FolderOpen size={14} aria-hidden="true" />
      <span>{projectName(repo)}</span>
    </div>
  );
}
