import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { DesktopTimelineBridge } from "./DesktopTimelineBridge";
import { DesktopUpdateControl } from "./DesktopUpdateControl";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { PermissionModeControl } from "./PermissionModeControl";
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
import "./desktop-tools.css";
import "./permission-mode.css";

interface ErrorBoundaryState {
  error?: Error;
}

/** Keep a renderer exception visible instead of leaving a blank desktop window. */
class DesktopErrorBoundary extends React.Component<React.PropsWithChildren, ErrorBoundaryState> {
  state: ErrorBoundaryState = {};

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("Medusa Desktop renderer failed", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <main style={{ minHeight: "100vh", display: "grid", placeItems: "center", padding: 32, fontFamily: "system-ui" }}>
        <section role="alert" style={{ maxWidth: 720 }}>
          <h1>Medusa Desktop needs attention</h1>
          <p>The renderer stopped while processing the request. The background runtime is still available for retry or restart.</p>
          <pre style={{ whiteSpace: "pre-wrap" }}>{this.state.error.message}</pre>
        </section>
      </main>
    );
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <DesktopErrorBoundary>
      <App />
      <PermissionModeControl />
      <DesktopTimelineBridge />
      <SessionDock />
      <DiffDock />
      <MemoryDock />
      <DesktopUpdateControl />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </DesktopErrorBoundary>
  </React.StrictMode>,
);
