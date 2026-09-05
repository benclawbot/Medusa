import React, { useMemo, useState } from "react";
import {
  loadProviderCatalog,
  startBrowserOauth,
  type ProviderCatalogEntry,
} from "./providerCatalog";
import type { Effort, SharedConfiguration } from "./runtime";
import { initialOnboardingStep, recommendedModels } from "./onboarding";

interface Props {
  configuration: SharedConfiguration;
  providers: ProviderCatalogEntry[];
  error?: string;
  onApply: (next: { provider: string; model: string; effort: Effort; apiKey?: string; baseUrl?: string }) => Promise<void>;
}

export function DesktopOnboarding({ configuration, providers, error, onApply }: Props) {
  const [catalog, setCatalog] = useState(providers);
  const [provider, setProvider] = useState(configuration.provider);
  const selectedProvider = useMemo(
    () => catalog.find((entry) => entry.profileProvider === provider),
    [catalog, provider],
  );
  const [model, setModel] = useState(configuration.model || selectedProvider?.defaultModel || "");
  const [effort, setEffort] = useState<Effort>(configuration.effort);
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(configuration.baseUrl ?? selectedProvider?.baseUrl ?? "");
  const [step, setStep] = useState(() => initialOnboardingStep(configuration, providers));
  const [saving, setSaving] = useState(false);
  const [authenticating, setAuthenticating] = useState(false);
  const [oauthConnected, setOauthConnected] = useState(configuration.credentialConfigured);
  const [authError, setAuthError] = useState<string>();
  const models = recommendedModels(selectedProvider);
  const credentialless = selectedProvider?.authMethods.every((method) => method === "none") ?? false;
  const oauth = selectedProvider?.browserOauth ?? false;
  const credentialReady = oauth
    ? oauthConnected
    : credentialless || configuration.credentialConfigured;

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
      setAuthError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setAuthenticating(false);
    }
  };

  const verify = async () => {
    setSaving(true);
    try {
      await onApply({ provider, model, effort, apiKey: apiKey.trim() || undefined, baseUrl: selectedProvider?.customValues ? baseUrl.trim() || undefined : undefined });
      setApiKey("");
      setStep("ready");
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
            <p>Connect an account, choose a model, and verify the shared profile before your first chat.</p>
            <small>Shared profile: {configuration.activeProfile}</small>
          </div>
        </div>

        <p role="note">
          New installs start in <strong>Ask for approval</strong>: Medusa can work inside your workspace,
          but asks before crossing protected boundaries such as using the internet or changing files
          outside it. You can deliberately choose a different permission profile after setup.
        </p>

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
            <h2>Preferences</h2>
            <label>Reasoning effort
              <select value={effort} onChange={(event) => setEffort(event.target.value as Effort)}>
                <option value="auto">Auto</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option>
              </select>
            </label>
            {selectedProvider?.customValues && (
              <label>Base URL
                <input type="url" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/v1" />
              </label>
            )}
            <div>
              <button className="secondary-action" onClick={() => setStep("model")}>Back</button>
              <button className="primary-action" onClick={() => setStep("verify")}>Review</button>
            </div>
          </>
        )}

        {step === "verify" && (
          <>
            <h2>Verify and start</h2>
            <p>{selectedProvider?.displayName} · {model} · {effort} effort</p>
            <small>Medusa will test the selected provider route and model before marking the session ready.</small>
            {!!error && <div className="error-banner" role="alert">{error}</div>}
            <div>
              <button className="secondary-action" disabled={saving} onClick={() => setStep("preferences")}>Back</button>
              <button className="primary-action" disabled={saving || !provider || !model} onClick={() => void verify()}>{saving ? "Verifying…" : "Verify configuration"}</button>
            </div>
          </>
        )}

        {step === "ready" && <p role="status">Configuration verified. Medusa is ready.</p>}
      </section>
    </main>
  );
}
