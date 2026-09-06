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

import { DesktopSlotsProvider, useDesktopSlots } from "./DesktopSlots";
import { DesktopUpdateControl } from "./DesktopUpdateControl";

function UpdateControlHarness() {
  const { updateRef } = useDesktopSlots();
  return (
    <>
      <div className="settings-form"><div ref={updateRef} data-medusa-updates /></div>
      <DesktopUpdateControl />
    </>
  );
}

describe("DesktopUpdateControl", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.progressListener = undefined;
    document.body.innerHTML = "";
  });

  it("renders installer progress events into the explicit settings slot", async () => {
    mocks.invoke.mockResolvedValueOnce({
      currentVersion: "1.0.1",
      latestMainSha: "0123456789abcdef0123456789abcdef01234567",
      executable: "C:\\Medusa\\medusa-desktop.exe",
      ready: true,
      artifactPublished: true,
    });
    mocks.invoke.mockResolvedValueOnce(undefined);

    render(<DesktopSlotsProvider><UpdateControlHarness /></DesktopSlotsProvider>);
    await waitFor(() => expect(screen.getByRole("button", { name: "Check main" })).toBeEnabled());
    expect(document.querySelector("[data-medusa-updates] [aria-label='Desktop updates']")).not.toBeNull();
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

    act(() => {
      mocks.progressListener?.({
        payload: {
          phase: "replacing",
          completed: 99,
          total: 100,
          message: "The update is ready. Medusa Desktop will close briefly while the application is replaced, then reopen automatically…",
        },
      });
    });
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "99");
    expect(screen.getByText(/close briefly while the application is replaced/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Closing to update…" })).toBeDisabled();
  });
});
