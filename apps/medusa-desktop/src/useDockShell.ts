import { useCallback, useRef, useState } from "react";
import { useDesktopToolRequest, type DesktopTool } from "./desktop-tools";
import { useDialogFocus } from "./useDialogFocus";

export function useDockShell<T extends HTMLElement>(tool: DesktopTool) {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string>();
  const dialogRef = useRef<T>(null);
  const close = useCallback(() => setOpen(false), []);
  const openFromRail = useCallback(() => setOpen(true), []);
  useDesktopToolRequest(tool, openFromRail);
  useDialogFocus(open, dialogRef, close);
  return { open, setOpen, close, error, setError, dialogRef };
}
