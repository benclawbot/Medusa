import { App as DesktopShell } from "./AppLegacy";
import { invoke } from "@tauri-apps/api/core";
import { useEffect } from "react";
import { DesktopTimelineBridge } from "./DesktopTimelineBridge";
import { DesktopUpdateControl } from "./DesktopUpdateControl";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { PermissionModeControl } from "./PermissionModeControl";

export * from "./AppLegacy";

/**
 * Single desktop composition owner. The shell and every auxiliary integration now belong to the
 * same React tree; `main.tsx` mounts only this component under StrictMode.
 */
export function App() {
  useEffect(() => {
    // Native Ready only means the window exists. A renderer-owned acknowledgement is written
    // after the shell has mounted so a blank/error boundary never commits a healthy update.
    void invoke("desktop_update_renderer_ready").catch(() => undefined);
  }, []);
  return (
    <>
      <DesktopShell
        settingsSlot={<DesktopUpdateControl />}
        composerSlot={<DesktopTimelineBridge />}
        composerToolsSlot={<PermissionModeControl />}
      />
      <DiffDock />
      <MemoryDock />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </>
  );
}
