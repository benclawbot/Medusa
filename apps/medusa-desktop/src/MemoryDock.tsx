import { BookOpen, X } from "lucide-react";
import { useEffect, useState } from "react";
import { MemoryBrowser } from "./MemoryBrowser";

export function MemoryDock() {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState(() => window.localStorage.getItem("medusa.desktop.repo") ?? "");

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
      <button className="memory-dock-trigger" onClick={() => setOpen(true)} aria-label="Open memory browser" title="Memory browser">
        <BookOpen size={17} />
      </button>
      {open && (
        <div className="memory-dock-backdrop" role="dialog" aria-modal="true" aria-label="Medusa memory browser">
          <section className="memory-dock-panel">
            <button className="memory-dock-close" onClick={() => setOpen(false)} aria-label="Close memory browser"><X size={17} /></button>
            <MemoryBrowser repo={repo} />
          </section>
        </div>
      )}
    </>
  );
}
