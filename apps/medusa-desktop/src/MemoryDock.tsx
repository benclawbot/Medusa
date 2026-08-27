import { X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { MemoryBrowser } from "./MemoryBrowser";
import { useDesktopToolRequest } from "./desktop-tools";
import { useDialogFocus } from "./useDialogFocus";

export function MemoryDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");
  const dialogRef = useRef<HTMLElement>(null);
  const close = useCallback(() => setOpen(false), []);
  const openFromRail = useCallback(() => setOpen(true), []);
  useDesktopToolRequest("memory", openFromRail);
  useDialogFocus(open, dialogRef, close);

  useEffect(() => {
    if (!open) return;
    const sync = () => setRepo(window.localStorage.getItem("medusa.desktop.repo") ?? "");
    sync();
    window.addEventListener("focus", sync);
    return () => {
      window.removeEventListener("focus", sync);
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
