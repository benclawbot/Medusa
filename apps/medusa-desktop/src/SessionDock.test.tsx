import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { formatSessionAge, SessionDock } from "./SessionDock";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

it("renders the compact recent-session rail without the old Sessions navigation", () => {
  render(<SessionDock />);

  expect(screen.getByRole("region", { name: "Recent sessions" })).toBeInTheDocument();
  expect(screen.getByText("Recent")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Sessions" })).not.toBeInTheDocument();
});

it("formats recent-session age without redundant relative-time copy", () => {
  const now = Date.parse("2026-09-08T12:00:00Z");
  expect(formatSessionAge("2026-09-08T11:00:00Z", now)).toBe("1h");
  expect(formatSessionAge("2026-09-07T12:00:00Z", now)).toBe("1d");
});
