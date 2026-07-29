import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { DesktopTimelineBridge } from "./DesktopTimelineBridge";
import { DesktopUpdateControl } from "./DesktopUpdateControl";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { MemoryDock } from "./MemoryDock";
import { RecoveryDock } from "./RecoveryDock";
import { SessionDock } from "./SessionDock";
import { VoiceControls } from "./VoiceControls";
import "./styles.css";
import "./medusa-desktop.css";
import "./recovery.css";
import "./diff-dock.css";
import "./reviewDiff.css";
import "./memory-browser.css";
import "./accessibility.css";
import "./desktop-ux-overhaul.css";
import "./desktop-timeline.css";
import "./structured-timeline.css";
import "./desktop-update.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
    <div className="desktop-voice-dock">
      <VoiceControls
        capability={{
          available: false,
          reason: "Voice is unavailable because the configured ChatGPT OAuth gateway does not expose an authenticated Realtime route. No microphone access was requested.",
        }}
      />
    </div>
    <DesktopTimelineBridge />
    <SessionDock />
    <DiffDock />
    <MemoryDock />
    <RecoveryDock />
    <DesktopUpdateControl />
    <EngineeringDashboardLauncher />
  </React.StrictMode>,
);
