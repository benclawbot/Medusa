import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { DESKTOP_TOOL_EVENT } from "./desktop-tools";
import { SessionDock } from "./SessionDock";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it.each([
  ["sessions", "Recent Medusa sessions"],
  ["review", "Code review"],
  ["memory", "Medusa memory browser"],
  ["learning", "Learning review"],
  ["engineering", "Engineering dashboard"],
] as const)("opens and dismisses the %s tool independently of repository context", async (tool, label) => {
  render(
    <>
      <SessionDock />
      <DiffDock />
      <MemoryDock />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </>,
  );

  window.dispatchEvent(new CustomEvent(DESKTOP_TOOL_EVENT, { detail: tool }));

  const dialog = await screen.findByRole("dialog", { name: label });
  expect(dialog).toBeInTheDocument();

  fireEvent.pointerDown(document.body);

  await waitFor(() => expect(screen.queryByRole("dialog", { name: label })).not.toBeInTheDocument());
});
