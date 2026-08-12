import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { DesktopTimelineBridge } from "./DesktopTimelineBridge";
import { DesktopUpdateControl } from "./DesktopUpdateControl";
import { DesktopVoiceDock } from "./DesktopVoiceDock";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { OpenAiRealtimeLiveEvidence } from "./OpenAiRealtimeLiveEvidence";
import { RecoveryDock } from "./RecoveryDock";
import { SessionDock } from "./SessionDock";
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
import "./codex-experience.css";
import "./neutral-light.css";
import "./mobile-navigation.css";

const liveEvidenceEnabled =
  import.meta.env.VITE_MEDUSA_OPENAI_REALTIME_EVIDENCE === "1" ||
  new URLSearchParams(window.location.search).get("openai-realtime-evidence") === "1";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {liveEvidenceEnabled ? (
      <OpenAiRealtimeLiveEvidence />
    ) : (
      <>
        <App />
        <DesktopVoiceDock />
        <DesktopTimelineBridge />
        <SessionDock />
        <DiffDock />
        <MemoryDock />
        <RecoveryDock />
        <DesktopUpdateControl />
        <LearningDock />
        <EngineeringDashboardLauncher />
      </>
    )}
  </React.StrictMode>,
);
