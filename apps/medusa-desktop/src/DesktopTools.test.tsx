import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { DiffDock } from "./DiffDock";
import { EngineeringDashboardLauncher } from "./EngineeringDashboard";
import { LearningDock } from "./LearningDock";
import { MemoryDock } from "./MemoryDock";
import { DESKTOP_TOOL_EVENT } from "./desktop-tools";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it.each([
  ["review", "Code review"],
  ["memory", "Medusa memory browser"],
  ["learning", "Learning review"],
  ["engineering", "Engineering dashboard"],
] as const)("opens and dismisses the %s tool independently of repository context", async (tool, label) => {
  render(
    <>
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

it.each([
  ["review", "Code review"],
  ["memory", "Medusa memory browser"],
  ["learning", "Learning review"],
  ["engineering", "Engineering dashboard"],
] as const)("closes the %s tool with Escape", async (tool, label) => {
  render(
    <>
      <DiffDock />
      <MemoryDock />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </>,
  );

  window.dispatchEvent(new CustomEvent(DESKTOP_TOOL_EVENT, { detail: tool }));
  const dialog = await screen.findByRole("dialog", { name: label });
  fireEvent.keyDown(dialog, { key: "Escape" });

  await waitFor(() => expect(screen.queryByRole("dialog", { name: label })).not.toBeInTheDocument());
});

it.each([
  ["review", "Code review", "Close code review"],
  ["memory", "Medusa memory browser", "Close memory browser"],
  ["learning", "Learning review", "Close learning review"],
  ["engineering", "Engineering dashboard", "Close engineering dashboard"],
] as const)("closes the %s tool with its close button", async (tool, label, closeLabel) => {
  render(
    <>
      <DiffDock />
      <MemoryDock />
      <LearningDock />
      <EngineeringDashboardLauncher />
    </>,
  );

  window.dispatchEvent(new CustomEvent(DESKTOP_TOOL_EVENT, { detail: tool }));
  await screen.findByRole("dialog", { name: label });
  fireEvent.click(screen.getByRole("button", { name: closeLabel }));

  await waitFor(() => expect(screen.queryByRole("dialog", { name: label })).not.toBeInTheDocument());
});
