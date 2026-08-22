import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { DESKTOP_TOOL_EVENT } from "./desktop-tools";
import { SessionDock } from "./SessionDock";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it("opens without a repository and closes when the user clicks outside", async () => {
  render(<SessionDock />);

  window.dispatchEvent(new CustomEvent(DESKTOP_TOOL_EVENT, { detail: "sessions" }));

  expect(await screen.findByRole("dialog", { name: "Recent Medusa sessions" })).toBeInTheDocument();
  expect(screen.getByText("No saved sessions for this project.")).toBeInTheDocument();

  fireEvent.pointerDown(document.body);

  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Recent Medusa sessions" })).not.toBeInTheDocument());
});
