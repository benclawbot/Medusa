import React, { useEffect, useMemo, useState } from "react";
import {
  loadProviderCatalog,
  startBrowserOauth,
  type ProviderCatalogEntry,
} from "./providerCatalog";
import { loadPermissionModes, setPermissionMode } from "./permissionModes";
import type { Effort, SharedConfiguration } from "./runtime";
import { initialOnboardingStep, recommendedModels } from "./onboarding";
import { toUserError } from "./errorPresentation";

interface Props {
  configuration: SharedConfiguration;
  providers: ProviderCatalogEntry[];
  error?: string;
  onApply: (next: { provider: string; model: string; effort: Effort; apiKey?: string; baseUrl?: string }) => Promise<void>;
}

const permissionOptions = [
  {
    id: "ask-for-approval",
    label: "Ask for approval",
    description: "Workspace access; ask before protected boundary actions such as outside-workspace changes or internet use.",
  },
  {
    id: "approve-for-me",
    label: "Approve for me",
    description: "Workspace access; automatically review routine requests and ask only for potentially unsafe actions.",
  },
  {
    id: "read-only",
    label: "Read Only",
    description: "Read workspace files; ask before edits or internet access.",
  },
  {
    id: "full-access",
    label: "Full Access",
    description: "Unrestricted file and internet access without routine approval prompts.",
  },
] as const;

export function DesktopOnboarding({ configuration, providers, error, onApply }: Props) {
  const [catalog, setCatalog] = useState(providers);
  const [provider, setProvider] = useState(configuration.provider);
  const selectedProvider = useMemo(
    () => catalog.find((entry) => entry.profileProvider === provider),
    [catalog, provider],
  );
  const [model, setModel] = useState(configuration.model || selectedProvider?.defaultModel || "");
  const [effort, setEffort] = useState<Effort>(configuration.effort);
  const [permissionMode, setPermissionModeSelection] = useState("");
  const [permissionModeDirty, setPermissionModeDirty] = useState(false);
  const [permissionError, setPermissionError] = useState<string>();
  const selectedPermission = permissionOptions.find((option) => option.id === permissionMode);
  const permissionsReady = Boolean(selectedPermission);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(configuration.baseUrl ?? selectedProvider?.baseUrl ?? "");
  const [step, setStep] = useState(() => initialOnboardingStep(configuration, providers));
  const [saving, setSaving] = useState(false);
  const [authenticating, setAuthenticating] = useState(false);
  const [oauthConnected, setOauthConnected] = useState(configuration.credentialConfigured);
  const [authError, setAuthError] = useState<string>();
  const [verifyError, setVerifyError] = useState<string>();
  const models = recommendedModels(selectedProvider);
  const credentialless = selectedProvider?.authMethods.every((method) => method === "none") ?? false;
  const oauth = selectedProvider?.browserOauth ?? false;
  const credentialReady = oauth
    ? oauthConnected
    : credentialless || configuration.credentialConfigured;

  useEffect(() => {
    let cancelled = false;
    void loadPermissionModes()
      .then((items) => {
        if (cancelled) return;
        const active = items.find((item) => item.active);
        if (!active || !permissionOptions.some((option) => option.id === active.id)) {
          setPermissionModeSelection("");
          setPermissionModeDirty(false);
          setPermissionError("Could not load model permissions: no supported active permission mode is available.");
          return;
        }
        setPermissionModeSelection(active.id);
        setPermissionModeDirty(false);
        setPermissionError(undefined);
      })
      .catch((cause) => {
        if (cancelled) return;
        setPermissionModeSelection("");
        setPermissionModeDirty(false);
        setPermissionError(`Could not load model permissions: ${toUserError(cause)}`);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const chooseProvider = (value: string) => {
    const next = catalog.find((entry) => entry.profileProvider === value);
    setProvider(value);
    setModel(next?.browserOauth ? "" : next?.defaultModel ?? "");
    setApiKey("");
    setBaseUrl(next?.baseUrl ?? "");
    setAuthError(undefined);
    setOauthConnected(value === configuration.provider && configuration.credentialConfigured);
  };

  const nextFromProvider = () => setStep(credentialReady ? "model" : "authentication");

  const authenticateWithBrowser = async () => {
    setAuthenticating(true);
    setAuthError(undefined);
    try {
      await startBrowserOauth(provider);
      const refreshed = await loadProviderCatalog(true, provider);
      setCatalog(refreshed);
      const refreshedProvider = refreshed.find((entry) => entry.profileProvider === provider);
      if (refreshedProvider) {
        const nextModels = recommendedModels(refreshedProvider);
        if (!nextModels.includes(model)) {
          setModel(nextModels[0] ?? refreshedProvider.defaultModel);
        }
      }
      setOauthConnected(true);
      setStep("model");
    } catch (cause) {
      setAuthError(toUserError(cause));
    } finally {
      setAuthenticating(false);
    }
  };

  const verify = async () => {
    if (!selectedPermission) {
      setPermissionError("Could not save model permissions: no permission mode is selected.");
      return;
    }
    setSaving(true);
    setPermissionError(undefined);
    setVerifyError(undefined);
    try {
      let permissionModeToSave = permissionMode;
      if (!permissionModeDirty) {
        try {
          const items = await loadPermissionModes();
          const active = items.find((item) => item.active);
          if (!active || !permissionOptions.some((option) => option.id === active.id)) {
            setPermissionError("Could not revalidate model permissions: no supported active permission mode is available.");
            return;
          }
          permissionModeToSave = active.id;
          setPermissionModeSelection(active.id);
        } catch (cause) {
          setPermissionError(`Could not revalidate model permissions: ${toUserError(cause)}`);
          return;
        }
      }
      try {
        await setPermissionMode(permissionModeToSave);
      } catch (cause) {
        setPermissionError(`Could not save model permissions: ${toUserError(cause)}`);
        return;
      }
      await onApply({ provider, model, effort, apiKey: apiKey.trim() || undefined, baseUrl: selectedProvider?.customValues ? baseUrl.trim() || undefined : undefined });
      setApiKey("");
      setStep("ready");
    } catch (cause) {
      setVerifyError(toUserError(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <main className="desktop-onboarding" aria-label="Medusa first-run setup">
      <section className="standalone-panel settings-form">
        <div className="panel-title">
          <div>
            <h1>Set up Medusa</h1>
            <p>Connect an account, choose a model, review permissions, and verify the shared profile before your first chat.</p>
            <small>Shared profile: {configuration.activeProfile}</small>
          </div>
        </div>

        {step === "provider" && (
          <>
            <h2>Choose a provider</h2>
            <label>Provider
              <select value={provider} onChange={(event) => chooseProvider(event.target.value)}>
                <option value="">Select a provider</option>
                {catalog.map((entry) => (
                  <option key={entry.id} value={entry.profileProvider} disabled={Boolean(entry.disabledReason)}>
                    {entry.displayName}
                  </option>
                ))}
              </select>
            </label>
            {selectedProvider && <p>{selectedProvider.disabledReason ?? selectedProvider.description}</p>}
            <button className="primary-action" disabled={!selectedProvider || Boolean(selectedProvider.disabledReason)} onClick={nextFromProvider}>Continue</button>
          </>
        )}

        {step === "authentication" && (
          <>
            <h2>Authenticate</h2>
            <p>{oauth ? "Sign in in your browser. Medusa will retain the local OAuth credential and refresh the models available to the authenticated account when sign-in completes." : "Enter an API key for this shared Medusa profile."}</p>
            {!oauth && !credentialless && (
              <label>API key
                <input type="password" autoComplete="off" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="API key" />
              </label>
            )}
            {oauth && !oauthConnected && <p role="status">Browser OAuth is not yet connected for this profile.</p>}
            {!!authError && <div className="error-banner" role="alert">{authError}</div>}
            <div>
              <button className="secondary-action" disabled={authenticating} onClick={() => setStep("provider")}>Back</button>
              {oauth && !oauthConnected ? (
                <button className="primary-action" disabled={authenticating} onClick={() => void authenticateWithBrowser()}>{authenticating ? "Opening browser…" : "Sign in with ChatGPT"}</button>
              ) : (
                <button className="primary-action" disabled={!credentialReady && !apiKey.trim()} onClick={() => setStep("model")}>Continue</button>
              )}
            </div>
          </>
        )}

        {step === "model" && (
          <>
            <h2>Choose a model</h2>
            <label>Model
              <select value={model} onChange={(event) => setModel(event.target.value)}>
                {models.map((id) => {
                  const metadata = selectedProvider?.models?.find((candidate) => candidate.id === id);
                  return <option key={id} value={id}>{metadata?.display_name || id}{metadata?.recommended ? " — Recommended" : ""}</option>;
                })}
              </select>
            </label>
            {selectedProvider?.models?.find((candidate) => candidate.id === model)?.use_case_hint && (
              <p>{selectedProvider.models.find((candidate) => candidate.id === model)?.use_case_hint}</p>
            )}
            <div>
              <button className="secondary-action" onClick={() => setStep(credentialReady ? "provider" : "authentication")}>Back</button>
              <button className="primary-action" disabled={!model.trim()} onClick={() => setStep("preferences")}>Continue</button>
            </div>
          </>
        )}

        {step === "preferences" && (
          <>
            <h2>Preferences and permissions</h2>
            <label>Reasoning effort
              <select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}>
                <option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option>
              </select>
            </label>
            <label>Model permissions
              <select
                value={permissionMode}
                disabled={!permissionsReady}
                onChange={(event) => {
                  setPermissionError(undefined);
                  setPermissionModeDirty(true);
                  setPermissionModeSelection(event.target.value);
                }}
              >
                {!permissionsReady && (
                  <option value="">{permissionError ? "Permissions unavailable" : "Loading permissions…"}</option>
                )}
                {permissionOptions.map((option) => (
                  <option key={option.id} value={option.id}>{option.label}</option>
                ))}
              </select>
            </label>
            {selectedPermission && <p>{selectedPermission.description}</p>}
            {!permissionError && !permissionsReady && <p role="status">Loading the current permission mode…</p>}
            {!!permissionError && <div className="error-banner" role="alert">{permissionError}</div>}
            {permissionMode === "full-access" && (
              <p role="alert">Full Access removes routine approval prompts and allows unrestricted file and internet access.</p>
            )}
            {selectedProvider?.customValues && (
              <label>Base URL
                <input type="url" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/v1" />
              </label>
            )}
            <div>
              <button className="secondary-action" onClick={() => setStep("model")}>Back</button>
              <button className="primary-action" disabled={!permissionsReady} onClick={() => setStep("verify")}>Review</button>
            </div>
          </>
        )}

        {step === "verify" && (
          <>
            <h2>Verify and start</h2>
            <p>{selectedProvider?.displayName} · {model} · {effort} effort</p>
            <p>Permissions: <strong>{selectedPermission?.label ?? "Loading permissions…"}</strong></p>
            <small>Medusa will save this permission choice and test the selected provider route and model before marking the session ready.</small>
            {!!(error ?? permissionError ?? verifyError) && <div className="error-banner" role="alert">{error ?? permissionError ?? verifyError}</div>}
            <div>
              <button className="secondary-action" disabled={saving} onClick={() => setStep("preferences")}>Back</button>
              <button className="primary-action" disabled={saving || !provider || !model || !permissionsReady} onClick={() => void verify()}>{saving ? "Verifying…" : "Verify configuration"}</button>
            </div>
          </>
        )}

        {step === "ready" && <p role="status">Configuration verified. Medusa is ready.</p>}
      </section>
    </main>
  );
}
