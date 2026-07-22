import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { DiffDock } from "./DiffDock";
import { MemoryDock } from "./MemoryDock";
import { SessionDock } from "./SessionDock";
import "./styles.css";
import "./medusa-desktop.css";
import "./diff-dock.css";
import "./memory-browser.css";
import "./accessibility.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
    <SessionDock />
    <DiffDock />
    <MemoryDock />
  </React.StrictMode>,
);
