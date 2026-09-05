import { invoke } from "@tauri-apps/api/core";

export const PERMISSION_MODE_CHANGED_EVENT = "medusa:permission-mode-changed";

export interface PermissionModeOption {
  id: string;
  label: string;
  description: string;
  active: boolean;
}

export async function loadPermissionModes(): Promise<PermissionModeOption[]> {
  return invoke<PermissionModeOption[]>("desktop_permission_modes");
}

export async function setPermissionMode(mode: string): Promise<PermissionModeOption> {
  const selected = await invoke<PermissionModeOption>("desktop_set_permission_mode", { mode });
  window.dispatchEvent(new Event(PERMISSION_MODE_CHANGED_EVENT));
  return selected;
}
