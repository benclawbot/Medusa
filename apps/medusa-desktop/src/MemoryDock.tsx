import { BookOpen, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { MemoryBrowser } from "./MemoryBrowser";
import { useDialogFocus } from "./useDialogFocus";

export function MemoryDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");
  const dialogRef = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDialogFocus(open, dialogRef, close);

  useEffect(() => {
    const sync = () => setRepo(window.localStorage.getItem("medusa.desktop.repo") ?? "");
    window.addEventListener("focus", sync);
    const interval = window.setInterval(sync, 500);
    return () => {
      window.removeEventListener("focus", sync);
      window.clearInterval(interval);
    };
  }, []);

  return (
    <>
      <button className="memory-dock-trigger" onClick={() => setOpen(true)} aria-label="Open memory browser" aria-haspopup="dialog" aria-expanded={open} title="Memory browser">
        <BookOpen size={17} />
      </button>
      {open && (
        <div ref={dialogRef} className="memory-dock-backdrop" role="dialog" aria-modal="true" aria-labelledby="memory-browser-title" tabIndex={-1}>
          <section className="memory-dock-panel">
            <h1 id="memory-browser-title" className="sr-only">Medusa memory browser</h1>
            <button className="memory-dock-close" onClick={close} aria-label="Close memory browser"><X size={17} /></button>
            <MemoryBrowser repo={repo} />
          </section>
        </div>
      )}
    </>
  );
}
