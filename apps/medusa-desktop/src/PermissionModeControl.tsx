import { BookOpen, Hand, ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toUserError } from "./errorPresentation";
import {
  loadPermissionModes,
  PERMISSION_MODE_CHANGED_EVENT,
  setPermissionMode,
  type PermissionModeOption,
} from "./permissionModes";

function modeCopy(mode: PermissionModeOption): { label: string; description: string } {
  switch (mode.id) {
    case "ask-for-approval":
      return {
        label: "Ask for approval",
        description: "Always ask to edit external files and use the internet",
      };
    case "approve-for-me":
      return {
        label: "Approve for me",
        description: "Only ask for actions detected as potentially unsafe",
      };
    case "full-access":
      return {
        label: "Full access",
        description: "Unrestricted access to the internet and any file on your computer",
      };
    case "read-only":
      return {
        label: "Read only",
        description: "Read files in the workspace; ask before edits or internet access",
      };
    default:
      return { label: mode.label, description: mode.description };
  }
}

function ModeIcon({ id }: { id: string }) {
  if (id === "ask-for-approval") return <Hand size={19} aria-hidden="true" />;
  if (id === "full-access") return <ShieldAlert size={19} aria-hidden="true" />;
  if (id === "read-only") return <BookOpen size={19} aria-hidden="true" />;
  return <ShieldCheck size={19} aria-hidden="true" />;
}

export function PermissionModeControl() {
  const [modes, setModes] = useState<PermissionModeOption[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    let generation = 0;
    const refresh = () => {
      const request = ++generation;
      setModes([]);
      setError(undefined);
      void loadPermissionModes()
        .then((items) => {
          if (!cancelled && request === generation) setModes(items);
        })
        .catch((reason) => {
          if (!cancelled && request === generation) setError(toUserError(reason));
        });
    };
    const handlePermissionModeChanged = () => refresh();

    refresh();
    window.addEventListener(PERMISSION_MODE_CHANGED_EVENT, handlePermissionModeChanged);
    return () => {
      cancelled = true;
      window.removeEventListener(PERMISSION_MODE_CHANGED_EVENT, handlePermissionModeChanged);
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
  const activeCopy = active ? modeCopy(active) : undefined;
  const display = active ?? {
    label: error ? "Permissions unavailable" : "Loading permissions…",
    description: error ? "The current permission mode could not be loaded." : "Loading the current permission mode.",
  };
  const displayCopy = activeCopy ?? display;

  const choose = async (id: string) => {
    if (busy || id === active?.id) {
      setOpen(false);
      return;
    }
    setBusy(true);
    setError(undefined);
    try {
      await setPermissionMode(id);
      setOpen(false);
    } catch (reason) {
      setError(toUserError(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="permission-mode-control" ref={rootRef}>
      <button
        className={`permission-mode-trigger${active?.id === "full-access" ? " full-access" : ""}`}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${displayCopy.label}: ${displayCopy.description}`}
        onClick={() => setOpen((value) => !value)}
      >
        {active ? <ModeIcon id={active.id} /> : <ShieldCheck size={16} aria-hidden="true" />}
        <span>{displayCopy.label}</span>
      </button>
      {open && (
        <div className="permission-mode-menu" role="menu" aria-label="How should ChatGPT actions be approved?">
          <div className="permission-mode-menu-header">
            <div className="permission-mode-heading">How should ChatGPT actions be approved?</div>
            <span className="permission-mode-learn-more">Learn more</span>
          </div>
          {modes.map((mode) => {
            const copy = modeCopy(mode);
            return (
              <button
                key={mode.id}
                type="button"
                role="menuitemradio"
                aria-checked={mode.active}
                className={`permission-mode-option${mode.active ? " active" : ""}${mode.id === "full-access" ? " danger" : ""}`}
                disabled={busy}
                onClick={() => void choose(mode.id)}
              >
                <span className="permission-mode-option-icon"><ModeIcon id={mode.id} /></span>
                <span className="permission-mode-option-copy">
                  <strong>{copy.label}</strong>
                  <small>{copy.description}</small>
                </span>
                <span className="permission-mode-option-mark">{mode.active ? "✓" : ""}</span>
              </button>
            );
          })}
          {!error && modes.length === 0 && <div role="status">Loading permission modes…</div>}
          {error && <div className="permission-mode-error" role="alert">{error}</div>}
        </div>
      )}
    </div>
  );
}
