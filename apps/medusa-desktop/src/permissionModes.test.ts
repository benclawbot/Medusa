import { invoke } from "@tauri-apps/api/core";
import { afterEach, expect, it, vi } from "vitest";
import { PERMISSION_MODE_CHANGED_EVENT, setPermissionMode } from "./permissionModes";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

afterEach(() => {
  vi.clearAllMocks();
});

it("notifies mounted permission controls after a successful permission change", async () => {
  vi.mocked(invoke).mockResolvedValue({
    id: "read-only",
    label: "Read Only",
    description: "Read workspace files; ask before edits or internet access.",
    active: true,
  });
  const listener = vi.fn();
  window.addEventListener(PERMISSION_MODE_CHANGED_EVENT, listener);

  try {
    await setPermissionMode("read-only");
    expect(invoke).toHaveBeenCalledWith("desktop_set_permission_mode", { mode: "read-only" });
    expect(listener).toHaveBeenCalledTimes(1);
  } finally {
    window.removeEventListener(PERMISSION_MODE_CHANGED_EVENT, listener);
  }
});

it("does not announce a permission change when persistence fails", async () => {
  vi.mocked(invoke).mockRejectedValue(new Error("permission store unavailable"));
  const listener = vi.fn();
  window.addEventListener(PERMISSION_MODE_CHANGED_EVENT, listener);

  try {
    await expect(setPermissionMode("full-access")).rejects.toThrow("permission store unavailable");
    expect(listener).not.toHaveBeenCalled();
  } finally {
    window.removeEventListener(PERMISSION_MODE_CHANGED_EVENT, listener);
  }
});
