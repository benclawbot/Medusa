import { ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  loadPermissionModes,
  setPermissionMode,
  type PermissionModeOption,
} from "./permissionModes";

function findComposerHost(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".composer-tools");
}

export function PermissionModeControl() {
  const [host, setHost] = useState<HTMLElement | null>(() => findComposerHost());
  const [modes, setModes] = useState<PermissionModeOption[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const refreshHost = () => setHost(findComposerHost());
    refreshHost();
    const observer = new MutationObserver(refreshHost);
    observer.observe(document.body, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;
    void loadPermissionModes()
      .then((items) => {
        if (!cancelled) setModes(items);
      })
      .catch((reason) => {
        if (!cancelled) setError(String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);

  const active = useMemo(() => modes.find((mode) => mode.active), [modes]);
  const display = active ?? {
    label: error ? "Permissions unavailable" : "Loading permissions…",
    description: error ? "The current permission mode could not be loaded." : "Loading the current permission mode.",
  };

  const choose = async (id: string) => {
    if (busy || id === active?.id) {
      setOpen(false);
      return;
    }
    setBusy(true);
    setError(undefined);
    try {
      const selected = await setPermissionMode(id);
      setModes((current) => current.map((mode) => ({ ...mode, active: mode.id === selected.id })));
      setOpen(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  if (!host) return null;

  return createPortal(
    <div className="permission-mode-control" ref={rootRef}>
      <button
        className={`permission-mode-trigger${active?.id === "full-access" ? " full-access" : ""}`}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${display.label}: ${display.description}`}
        onClick={() => setOpen((value) => !value)}
      >
        {active?.id === "full-access" ? <ShieldAlert size={16} /> : <ShieldCheck size={16} />}
        <span>{display.label}</span>
      </button>
      {open && (
        <div className="permission-mode-menu" role="menu" aria-label="Model permissions">
          <div className="permission-mode-heading">Model permissions</div>
          {modes.map((mode) => (
            <button
              key={mode.id}
              type="button"
              role="menuitemradio"
              aria-checked={mode.active}
              className={`permission-mode-option${mode.active ? " active" : ""}${mode.id === "full-access" ? " danger" : ""}`}
              disabled={busy}
              onClick={() => void choose(mode.id)}
            >
              <span className="permission-mode-option-mark">{mode.active ? "✓" : ""}</span>
              <span className="permission-mode-option-copy">
                <strong>{mode.label}</strong>
                <small>{mode.description}</small>
              </span>
            </button>
          ))}
          {!error && modes.length === 0 && <div role="status">Loading permission modes…</div>}
          {error && <div className="permission-mode-error" role="alert">{error}</div>}
        </div>
      )}
    </div>,
    host,
  );
}
