import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { MemoryBrowser } from "./MemoryBrowser";
import { useDockShell } from "./useDockShell";
import { REPO_CHANGED_EVENT } from "./runtime";

export function MemoryDock() {
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");
  const { open, setOpen, close, dialogRef } = useDockShell<HTMLElement>("memory");

  useEffect(() => {
    if (!open) return;
    const sync = () => setRepo(window.localStorage.getItem("medusa.desktop.repo") ?? "");
    sync();
    window.addEventListener("focus", sync);
    window.addEventListener(REPO_CHANGED_EVENT, sync);
    return () => {
      window.removeEventListener("focus", sync);
      window.removeEventListener(REPO_CHANGED_EVENT, sync);
    };
  }, [open]);

  return (
    <>
      {open && (
        <div className="memory-dock-backdrop">
          <section ref={dialogRef} className="memory-dock-panel" role="dialog" aria-modal="true" aria-labelledby="memory-browser-title" tabIndex={-1}>
            <h1 id="memory-browser-title" className="sr-only">Medusa memory browser</h1>
            <button className="memory-dock-close" onClick={close} aria-label="Close memory browser"><X size={17} /></button>
            <MemoryBrowser repo={repo} />
          </section>
        </div>
      )}
    </>
  );
}
