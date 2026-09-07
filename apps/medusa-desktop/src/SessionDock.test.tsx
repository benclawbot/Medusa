import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { SessionDock } from "./SessionDock";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it("renders expanded inline without a repository and collapses beneath Sessions", () => {
  render(<SessionDock />);

  expect(screen.getByRole("region", { name: "Sessions" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sessions" })).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByText("No saved sessions for this project.")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
  expect(screen.getByRole("button", { name: "Sessions" })).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByText("No saved sessions for this project.")).not.toBeInTheDocument();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});
