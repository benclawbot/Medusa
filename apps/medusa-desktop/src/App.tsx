import { App as DesktopShell } from "./AppLegacy";
import { DesktopTimelineBridge } from "./DesktopTimelineBridge";
import { DesktopUpdateControl } from "./DesktopUpdateControl";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { PermissionModeControl } from "./PermissionModeControl";
import { SessionDock } from "./SessionDock";

export * from "./AppLegacy";

/**
 * Single desktop composition owner. The shell and every auxiliary integration now belong to the
 * same React tree; `main.tsx` mounts only this component under StrictMode.
 */
export function App() {
  return (
    <>
      <DesktopShell />
      <PermissionModeControl />
      <DesktopTimelineBridge />
      <SessionDock />
      <DiffDock />
      <MemoryDock />
      <DesktopUpdateControl />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </>
  );
}
