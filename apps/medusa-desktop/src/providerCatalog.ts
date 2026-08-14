import { invoke } from "@tauri-apps/api/core";

export interface ProviderCatalogEntry {
  id: string;
  displayName: string;
  description: string;
  connection: string;
  profileProvider: string;
  authMethods: string[];
  defaultAuth: string;
  defaultModel: string;
  modelOptions: string[];
  baseUrl?: string;
  browserOauth: boolean;
  discoverModels: boolean;
  customValues: boolean;
  disabledReason?: string;
  currentCustom: boolean;
}

export async function loadProviderCatalog(): Promise<ProviderCatalogEntry[]> {
  return invoke<ProviderCatalogEntry[]>("desktop_provider_catalog");
}
