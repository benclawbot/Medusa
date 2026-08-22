import { useEffect } from "react";

export const DESKTOP_TOOL_EVENT = "medusa:open-tool";

export type DesktopTool = "sessions" | "review" | "memory" | "learning" | "engineering";

export function requestDesktopTool(tool: DesktopTool): void {
  window.dispatchEvent(new CustomEvent<DesktopTool>(DESKTOP_TOOL_EVENT, { detail: tool }));
}

export function useDesktopToolRequest(tool: DesktopTool, onRequest: () => void): void {
  useEffect(() => {
    const handleRequest = (event: Event) => {
      if ((event as CustomEvent<DesktopTool>).detail === tool) onRequest();
    };
    window.addEventListener(DESKTOP_TOOL_EVENT, handleRequest);
    return () => window.removeEventListener(DESKTOP_TOOL_EVENT, handleRequest);
  }, [onRequest, tool]);
}
