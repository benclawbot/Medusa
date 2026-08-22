import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  progressListener: undefined as ((event: { payload: unknown }) => void) | undefined,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_event: string, listener: (event: { payload: unknown }) => void) => {
    mocks.progressListener = listener;
    return Promise.resolve(() => undefined);
  }),
}));

import { DesktopUpdateControl } from "./DesktopUpdateControl";

describe("DesktopUpdateControl", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.progressListener = undefined;
    document.body.innerHTML = '<div class="settings-form"></div>';
  });

  it("renders installer progress events instead of exposing a terminal build", async () => {
    mocks.invoke.mockResolvedValueOnce({
      currentVersion: "1.0.1",
      latestMainSha: "0123456789abcdef0123456789abcdef01234567",
      executable: "C:\\Medusa\\medusa-desktop.exe",
      ready: true,
      artifactPublished: true,
    });
    mocks.invoke.mockResolvedValueOnce(undefined);

    render(<DesktopUpdateControl />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Check main" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "Check main" }));
    await waitFor(() => expect(screen.getByText(/Checked main:/)).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Update and restart" }));
    await waitFor(() => expect(mocks.invoke).toHaveBeenLastCalledWith(
      "desktop_update_from_main",
      { targetSha: "0123456789abcdef0123456789abcdef01234567" },
    ));

    act(() => {
      mocks.progressListener?.({
        payload: {
          phase: "downloading",
          completed: 50,
          total: 100,
          message: "Downloading the verified desktop executable…",
        },
      });
    });
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
    expect(screen.getByText(/Downloading the verified desktop executable/)).toBeInTheDocument();
  });
});
