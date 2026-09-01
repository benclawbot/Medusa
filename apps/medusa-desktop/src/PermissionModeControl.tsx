import { invoke } from "@tauri-apps/api/core";
import { ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

interface PermissionModeOption {
  id: string;
  label: string;
  description: string;
  active: boolean;
}

async function loadPermissionModes(): Promise<PermissionModeOption[]> {
  return invoke<PermissionModeOption[]>("desktop_permission_modes");
}

async function setPermissionMode(mode: string): Promise<PermissionModeOption> {
  return invoke<PermissionModeOption>("desktop_set_permission_mode", { mode });
}

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

  const active = useMemo(
    () => modes.find((mode) => mode.active) ?? {
      id: "full-access",
      label: "Full Access",
      description: "Unrestricted access to the internet and files on this computer.",
      active: true,
    },
    [modes],
  );

  const choose = async (id: string) => {
    if (busy || id === active.id) {
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
        className={`permission-mode-trigger${active.id === "full-access" ? " full-access" : ""}`}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${active.label}: ${active.description}`}
        onClick={() => setOpen((value) => !value)}
      >
        {active.id === "full-access" ? <ShieldAlert size={16} /> : <ShieldCheck size={16} />}
        <span>{active.label}</span>
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
          {error && <div className="permission-mode-error" role="alert">{error}</div>}
        </div>
      )}
    </div>,
    host,
  );
}
