import { invoke } from "@tauri-apps/api/core";

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
  return invoke<PermissionModeOption>("desktop_set_permission_mode", { mode });
}
