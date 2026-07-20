import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { SessionDock } from "./SessionDock";
import "./styles.css";
import "./medusa-desktop.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
    <SessionDock />
  </React.StrictMode>,
);
