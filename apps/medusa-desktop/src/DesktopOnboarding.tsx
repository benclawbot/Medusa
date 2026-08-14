import React, { useMemo, useState } from "react";
import type { ProviderCatalogEntry } from "./providerCatalog";
import type { Effort, SharedConfiguration } from "./runtime";
import { initialOnboardingStep, recommendedModels } from "./onboarding";

interface Props {
  configuration: SharedConfiguration;
  providers: ProviderCatalogEntry[];
  error?: string;
  onApply: (next: { provider: string; model: string; effort: Effort; apiKey?: string }) => Promise<void>;
}

export function DesktopOnboarding({ configuration, providers, error, onApply }: Props) {
  const [provider, setProvider] = useState(configuration.provider);
  const selectedProvider = useMemo(
    () => providers.find((entry) => entry.profileProvider === provider),
    [providers, provider],
  );
  const [model, setModel] = useState(configuration.model || selectedProvider?.defaultModel || "");
  const [effort, setEffort] = useState<Effort>(configuration.effort);
  const [apiKey, setApiKey] = useState("");
  const [step, setStep] = useState(() => initialOnboardingStep(configuration, providers));
  const [saving, setSaving] = useState(false);
  const models = recommendedModels(selectedProvider);
  const credentialless = selectedProvider?.authMethods.every((method) => method === "none") ?? false;
  const oauth = selectedProvider?.browserOauth ?? false;

  const chooseProvider = (value: string) => {
    const next = providers.find((entry) => entry.profileProvider === value);
    setProvider(value);
    setModel(next?.defaultModel ?? "");
    setApiKey("");
  };

  const nextFromProvider = () => setStep(credentialless || configuration.credentialConfigured ? "model" : "authentication");

  const verify = async () => {
    setSaving(true);
    try {
      await onApply({ provider, model, effort, apiKey: apiKey.trim() || undefined });
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

        {step === "provider" && (
          <>
            <h2>Choose a provider</h2>
            <label>Provider
              <select value={provider} onChange={(event) => chooseProvider(event.target.value)}>
                <option value="">Select a provider</option>
                {providers.map((entry) => (
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
            <p>{oauth ? "This provider supports browser OAuth. Existing shared OAuth credentials are reused automatically when present." : "Enter an API key for this shared Medusa profile."}</p>
            {!oauth && !credentialless && (
              <label>API key
                <input type="password" autoComplete="off" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="API key" />
              </label>
            )}
            {oauth && !configuration.credentialConfigured && <p role="status">Browser OAuth is not yet connected for this profile.</p>}
            <div>
              <button className="secondary-action" onClick={() => setStep("provider")}>Back</button>
              <button className="primary-action" disabled={oauth ? !configuration.credentialConfigured : !credentialless && !apiKey.trim()} onClick={() => setStep("model")}>Continue</button>
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
              <button className="secondary-action" onClick={() => setStep(credentialless || configuration.credentialConfigured ? "provider" : "authentication")}>Back</button>
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
